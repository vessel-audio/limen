//! The X server as the window manager and input devices see it: XTest
//! pointer and keyboard input, the clipboard, root-window pixels, and the
//! requests the application sends to its window manager.
//!
//! The control panel keeps server-side decorations on X11, so minimize,
//! maximize, and close buttons belong to the window manager. No window
//! manager runs here; the suite plays its part by observing the requests
//! sent to the root window and by sending `WM_DELETE_WINDOW` itself.

use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    self, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _,
    CreateWindowAux, EventMask, ImageFormat, InputFocus, Window, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// X keysyms used by the cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key(pub u32);

impl Key {
    pub const BACKSPACE: Self = Self(0xff08);
    pub const TAB: Self = Self(0xff09);
    pub const RETURN: Self = Self(0xff0d);
    pub const ESCAPE: Self = Self(0xff1b);
    pub const SHIFT: Self = Self(0xffe1);
    pub const CONTROL: Self = Self(0xffe3);
    pub const ALT: Self = Self(0xffe9);

    /// The keysym of a printable ASCII character.
    pub fn char(character: char) -> Self {
        Self(character as u32)
    }
}

/// Mouse buttons in X numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left = 1,
    WheelUp = 4,
    WheelDown = 5,
}

/// A client message the application sent to the window manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WmRequest {
    pub kind: String,
    pub data: [u32; 5],
}

/// Connections to one private X server.
pub struct X11 {
    input: RustConnection,
    root: Window,
    width: u16,
    height: u16,
    requestor: Window,
    keymap: Vec<Vec<u32>>,
    min_keycode: u8,
    observer: RustConnection,
    requests: RefCell<Vec<WmRequest>>,
}

impl X11 {
    /// Connect and start observing window-manager requests.
    pub fn connect(display: &str) -> Result<Self> {
        let (input, screen) = RustConnection::connect(Some(display))?;
        let setup = input.setup();
        let root = setup.roots[screen].root;
        let (width, height) = (
            setup.roots[screen].width_in_pixels,
            setup.roots[screen].height_in_pixels,
        );
        let (min_keycode, max_keycode) = (setup.min_keycode, setup.max_keycode);
        let mapping = input
            .get_keyboard_mapping(min_keycode, max_keycode - min_keycode + 1)?
            .reply()?;
        let keymap = mapping
            .keysyms
            .chunks(usize::from(mapping.keysyms_per_keycode))
            .map(<[u32]>::to_vec)
            .collect();
        let requestor = input.generate_id()?;
        input.create_window(
            0,
            requestor,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )?;

        let (observer, _) = RustConnection::connect(Some(display))?;
        observer.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
        )?;
        observer.sync()?;
        Ok(Self {
            input,
            root,
            width,
            height,
            requestor,
            keymap,
            min_keycode,
            observer,
            requests: RefCell::new(Vec::new()),
        })
    }

    pub fn move_to(&self, x: i32, y: i32) -> Result<()> {
        self.fake(xproto::MOTION_NOTIFY_EVENT, 0, x, y)?;
        std::thread::sleep(Duration::from_millis(30));
        Ok(())
    }

    pub fn press(&self, button: Button) -> Result<()> {
        self.fake(xproto::BUTTON_PRESS_EVENT, button as u8, 0, 0)
    }

    pub fn release(&self, button: Button) -> Result<()> {
        self.fake(xproto::BUTTON_RELEASE_EVENT, button as u8, 0, 0)
    }

    pub fn click(&self, x: i32, y: i32) -> Result<()> {
        self.move_to(x, y)?;
        self.press(Button::Left)?;
        self.release(Button::Left)?;
        std::thread::sleep(Duration::from_millis(30));
        Ok(())
    }

    /// Press at one point, move in steps, and release at another.
    pub fn drag(&self, from: (i32, i32), to: (i32, i32)) -> Result<()> {
        self.move_to(from.0, from.1)?;
        self.press(Button::Left)?;
        for step in 1..=5 {
            self.move_to(
                from.0 + (to.0 - from.0) * step / 5,
                from.1 + (to.1 - from.1) * step / 5,
            )?;
        }
        self.release(Button::Left)
    }

    /// Turn the wheel at a point; each notch is one press and release.
    pub fn scroll(&self, x: i32, y: i32, button: Button, notches: u32) -> Result<()> {
        self.move_to(x, y)?;
        for _ in 0..notches {
            self.press(button)?;
            self.release(button)?;
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// Press keys in order, then release them in reverse: a chord.
    pub fn chord(&self, keys: &[Key]) -> Result<()> {
        let codes = keys
            .iter()
            .map(|key| self.keycode(*key).map(|(code, _)| code))
            .collect::<Result<Vec<_>>>()?;
        for code in &codes {
            self.fake(xproto::KEY_PRESS_EVENT, *code, 0, 0)?;
        }
        for code in codes.iter().rev() {
            self.fake(xproto::KEY_RELEASE_EVENT, *code, 0, 0)?;
        }
        std::thread::sleep(Duration::from_millis(30));
        Ok(())
    }

    /// Type printable ASCII text, holding Shift where the keymap needs it.
    pub fn type_text(&self, text: &str) -> Result<()> {
        for character in text.chars() {
            let key = Key::char(character);
            match self.keycode(key)? {
                (_, true) => self.chord(&[Key::SHIFT, key])?,
                (_, false) => self.chord(&[key])?,
            }
        }
        Ok(())
    }

    /// The keycode producing a keysym and whether it needs Shift.
    fn keycode(&self, key: Key) -> Result<(u8, bool)> {
        self.keymap
            .iter()
            .enumerate()
            .find_map(|(index, syms)| {
                syms.iter()
                    .take(2)
                    .position(|sym| *sym == key.0)
                    .map(|level| (self.min_keycode + index as u8, level == 1))
            })
            .with_context(|| format!("no keycode produces keysym {:#x}", key.0))
    }

    fn fake(&self, kind: u8, detail: u8, x: i32, y: i32) -> Result<()> {
        self.input.xtest_fake_input(
            kind,
            detail,
            x11rb::CURRENT_TIME,
            self.root,
            x as i16,
            y as i16,
            0,
        )?;
        self.input.sync()?;
        Ok(())
    }

    /// The CLIPBOARD selection as UTF-8 text, if any client owns it.
    pub fn clipboard(&self) -> Result<Option<String>> {
        let atom = |name: &str| -> Result<u32> {
            Ok(self
                .input
                .intern_atom(false, name.as_bytes())?
                .reply()?
                .atom)
        };
        let (clipboard, utf8, property) = (
            atom("CLIPBOARD")?,
            atom("UTF8_STRING")?,
            atom("E2E_CLIPBOARD")?,
        );
        if self.input.get_selection_owner(clipboard)?.reply()?.owner == x11rb::NONE {
            return Ok(None);
        }
        self.input.convert_selection(
            self.requestor,
            clipboard,
            utf8,
            property,
            x11rb::CURRENT_TIME,
        )?;
        self.input.flush()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.input.poll_for_event()? {
                Some(Event::SelectionNotify(event)) if event.property == x11rb::NONE => {
                    return Ok(None);
                }
                Some(Event::SelectionNotify(_)) => {
                    let reply = self
                        .input
                        .get_property(
                            true,
                            self.requestor,
                            property,
                            AtomEnum::ANY,
                            0,
                            u32::MAX / 4,
                        )?
                        .reply()?;
                    return Ok(Some(String::from_utf8(reply.value)?));
                }
                Some(_) => {}
                None if Instant::now() > deadline => bail!("the clipboard owner did not answer"),
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    /// The color of one screen pixel as RGB.
    pub fn pixel(&self, x: i32, y: i32) -> Result<[u8; 3]> {
        let image = self
            .input
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                x as i16,
                y as i16,
                1,
                1,
                !0,
            )?
            .reply()?;
        match image.data.as_slice() {
            [blue, green, red, ..] => Ok([*red, *green, *blue]),
            _ => bail!("empty image"),
        }
    }

    /// Save the whole screen as a PNG.
    pub fn screenshot(&self, path: &Path) -> Result<()> {
        let image = self
            .input
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                0,
                0,
                self.width,
                self.height,
                !0,
            )?
            .reply()?;
        let rgb = image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|[blue, green, red, _]| [*red, *green, *blue])
            .collect::<Vec<_>>();
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(std::fs::File::create(path)?),
            u32::from(self.width),
            u32::from(self.height),
        );
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgb)?;
        Ok(())
    }

    pub fn atom(&self, name: &str) -> Result<u32> {
        Ok(self
            .input
            .intern_atom(false, name.as_bytes())?
            .reply()?
            .atom)
    }

    /// Give the window with this `WM_CLASS` the keyboard focus, as a window
    /// manager does when it maps a window; the application only counts as
    /// active, and reports focus to assistive technology, once it has it.
    pub fn focus_window(&self, class: &str) -> Result<()> {
        self.input
            .set_input_focus(InputFocus::PARENT, self.window(class)?, x11rb::CURRENT_TIME)?;
        self.input.sync()?;
        Ok(())
    }

    /// Ask the window with this `WM_CLASS` to close, as a window manager's
    /// close button does.
    pub fn close_window(&self, class: &str) -> Result<()> {
        let (protocols, delete) = (self.atom("WM_PROTOCOLS")?, self.atom("WM_DELETE_WINDOW")?);
        let window = self.window(class)?;
        self.input.send_event(
            false,
            window,
            EventMask::NO_EVENT,
            ClientMessageEvent::new(
                32,
                window,
                protocols,
                [delete, x11rb::CURRENT_TIME, 0, 0, 0],
            ),
        )?;
        self.input.sync()?;
        Ok(())
    }

    /// The top-level window with this `WM_CLASS`.
    fn window(&self, class: &str) -> Result<Window> {
        self.input
            .query_tree(self.root)?
            .reply()?
            .children
            .into_iter()
            .find(|window| {
                self.input
                    .get_property(false, *window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .is_some_and(|reply| {
                        reply
                            .value
                            .split(|byte| *byte == 0)
                            .any(|part| part == class.as_bytes())
                    })
            })
            .with_context(|| format!("no window has WM_CLASS {class}"))
    }

    /// Every window-manager request received so far.
    pub fn wm_requests(&self) -> Result<Vec<WmRequest>> {
        while let Some(event) = self.observer.poll_for_event()? {
            if let Event::ClientMessage(message) = event {
                let kind = self.observer.get_atom_name(message.type_)?.reply()?.name;
                self.requests.borrow_mut().push(WmRequest {
                    kind: String::from_utf8(kind)?,
                    data: message.data.as_data32(),
                });
            }
        }
        Ok(self.requests.borrow().clone())
    }
}
