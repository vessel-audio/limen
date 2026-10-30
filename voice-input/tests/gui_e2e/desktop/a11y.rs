//! AT-SPI queries against the private accessibility bus.

use anyhow::{Context, Result};
use atspi::proxy::accessible::AccessibleProxyBlocking;
use atspi::proxy::component::ComponentProxyBlocking;
use atspi::{CoordType, ObjectRefOwned, Role, State, StateSet};
use zbus::blocking::Connection;
use zbus::proxy::CacheProperties;

const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT: &str = "/org/a11y/atspi/accessible/root";

/// A client of the AT-SPI registry.
pub struct A11y {
    connection: Connection,
}

/// One accessible object as assistive technology sees it.
#[derive(Debug, Clone)]
pub struct Node {
    /// The stable accessible ID, empty when the element sets none.
    pub id: String,
    pub name: String,
    pub description: String,
    pub role: Role,
    pub states: StateSet,
    /// Screen bounds, absent for objects without a component.
    pub bounds: Option<Bounds>,
    /// The index of the parent in the same snapshot.
    pub parent: Option<usize>,
}

/// A screen rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Bounds {
    pub fn center(self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }

    pub fn bottom(self) -> i32 {
        self.y + self.height
    }
}

impl Node {
    pub fn has(&self, state: State) -> bool {
        self.states.contains(state)
    }
}

/// The bounds of the nearest scroll view around `nodes[index]`. AccessKit
/// reports scroll views to AT-SPI as panels.
pub fn viewport(nodes: &[Node], index: usize) -> Option<Bounds> {
    std::iter::successors(nodes[index].parent, |parent| nodes[*parent].parent)
        .find(|ancestor| nodes[*ancestor].role == Role::Panel)
        .and_then(|ancestor| nodes[ancestor].bounds)
}

impl A11y {
    pub fn connect(address: &str) -> Result<Self> {
        let connection = zbus::blocking::connection::Builder::address(address)?
            .build()
            .context("cannot connect to the accessibility bus")?;
        Ok(Self { connection })
    }

    /// Every object of every registered application, depth first.
    pub fn nodes(&self) -> Result<Vec<Node>> {
        let root = self.accessible(REGISTRY, ROOT)?;
        let mut nodes = Vec::new();
        for application in root.get_children()? {
            self.collect(&application, None, &mut nodes)?;
        }
        Ok(nodes)
    }

    fn collect(
        &self,
        object: &ObjectRefOwned,
        parent: Option<usize>,
        nodes: &mut Vec<Node>,
    ) -> Result<()> {
        let Some(destination) = object.name_as_str() else {
            return Ok(());
        };
        let path = object.path_as_str();
        let accessible = self.accessible(destination, path)?;
        let bounds = ComponentProxyBlocking::builder(&self.connection)
            .destination(destination)?
            .path(path)?
            .cache_properties(CacheProperties::No)
            .build()?
            .get_extents(CoordType::Screen)
            .ok()
            .map(|(x, y, width, height)| Bounds {
                x,
                y,
                width,
                height,
            })
            .filter(|bounds| bounds.width > 0 && bounds.height > 0);
        nodes.push(Node {
            id: accessible.accessible_id().unwrap_or_default(),
            name: accessible.name()?,
            description: accessible.description().unwrap_or_default(),
            role: accessible.get_role()?,
            states: accessible.get_state()?,
            bounds,
            parent,
        });
        let index = nodes.len() - 1;
        for child in accessible.get_children()? {
            self.collect(&child, Some(index), nodes)?;
        }
        Ok(())
    }

    fn accessible<'a>(
        &'a self,
        destination: &'a str,
        path: &'a str,
    ) -> Result<AccessibleProxyBlocking<'a>> {
        Ok(AccessibleProxyBlocking::builder(&self.connection)
            .destination(destination)?
            .path(path)?
            .cache_properties(CacheProperties::No)
            .build()?)
    }
}

/// An indented dump of the tree, attached to failures.
pub fn describe(nodes: &[Node]) -> String {
    nodes
        .iter()
        .map(|node| {
            format!(
                "{:?} {:?} id={:?} bounds={:?} states={:?}",
                node.role,
                node.name,
                node.id,
                node.bounds,
                node.states.iter().collect::<Vec<_>>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
