//! Which of a plugin's buses to activate.

use vst_audio::{Bus, BusChoice, BusConfiguration, BusInfo, BusKind, layout};

use super::host::Channels;
use crate::Sidechain;

/// The main output bus, and the main input bus followed by the side-chain
/// buses when asked for. The processor's channels are the active buses'
/// channels in order, so the main bus must come first.
pub(super) fn choose(
    inputs: &[BusInfo],
    outputs: &[BusInfo],
    channels: Channels,
    sidechain: Sidechain,
) -> Result<BusChoice, String> {
    let first_main = |buses: &[BusInfo]| buses.iter().position(|bus| bus.kind == BusKind::Main);
    let main_input = first_main(inputs);
    if let Some(position) = main_input.filter(|position| *position != 0) {
        return Err(format!(
            "the main input bus is bus {position}; a side-chain bus comes first"
        ));
    }
    let arranged = |bus: &BusInfo| match channels {
        Channels::PreferMono => layout::default_arrangement(1)
            .map(|arrangement| BusConfiguration {
                layout: Bus {
                    channels: 1,
                    arrangement,
                },
                active: true,
            })
            .map_err(|error| error.to_string()),
        Channels::AsDeclared => Ok(bus.configuration(true)),
    };
    let input_choice = |index: usize, bus: &BusInfo| match (bus.kind, sidechain) {
        (BusKind::Main, _) if Some(index) == main_input => arranged(bus),
        (BusKind::Aux, Sidechain::On) => Ok(bus.configuration(true)),
        (BusKind::Main | BusKind::Aux, Sidechain::On | Sidechain::Off) => {
            Ok(bus.configuration(false))
        }
    };
    let output_main = first_main(outputs);
    Ok(BusChoice {
        inputs: inputs
            .iter()
            .enumerate()
            .map(|(index, bus)| input_choice(index, bus))
            .collect::<Result<_, String>>()?,
        outputs: outputs
            .iter()
            .enumerate()
            .map(|(index, bus)| match Some(index) == output_main {
                true => arranged(bus),
                false => Ok(bus.configuration(false)),
            })
            .collect::<Result<_, String>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus(kind: BusKind, channels: u16) -> BusInfo {
        BusInfo {
            layout: Bus {
                channels,
                arrangement: layout::default_arrangement(channels).unwrap(),
            },
            kind,
            name: String::new(),
            default_active: kind == BusKind::Main,
        }
    }

    /// The active buses' channel counts per direction.
    fn active(choice: &BusChoice) -> (Vec<u16>, Vec<u16>) {
        let counts = |buses: &[BusConfiguration]| {
            buses
                .iter()
                .filter(|bus| bus.active)
                .map(|bus| bus.layout.channels)
                .collect()
        };
        (counts(&choice.inputs), counts(&choice.outputs))
    }

    #[test]
    fn a_compressor_gets_its_main_bus_and_its_sidechain_only_when_asked() {
        let inputs = [bus(BusKind::Main, 2), bus(BusKind::Aux, 2)];
        let outputs = [bus(BusKind::Main, 2)];
        let chosen =
            |channels, sidechain| active(&choose(&inputs, &outputs, channels, sidechain).unwrap());
        assert_eq!(
            [
                chosen(Channels::AsDeclared, Sidechain::Off),
                chosen(Channels::AsDeclared, Sidechain::On),
                chosen(Channels::PreferMono, Sidechain::On),
            ],
            [
                (vec![2], vec![2]),
                (vec![2, 2], vec![2]),
                (vec![1, 2], vec![1]),
            ]
        );
    }

    #[test]
    fn a_sidechain_bus_ahead_of_the_main_one_is_refused() {
        let inputs = [bus(BusKind::Aux, 2), bus(BusKind::Main, 2)];
        assert_eq!(
            choose(
                &inputs,
                &[bus(BusKind::Main, 2)],
                Channels::AsDeclared,
                Sidechain::Off
            )
            .err(),
            Some("the main input bus is bus 1; a side-chain bus comes first".to_string())
        );
    }
}
