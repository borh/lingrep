use anstyle::{Ansi256Color, AnsiColor, Color, RgbColor};

use crate::presentation::ColorMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextStyle {
    Proximity(Color),
    Distant,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ContextColorTheme {
    #[default]
    Auto,
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedContextColorTheme {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalColorCapability {
    Truecolor,
    Ansi256,
    Ansi16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextColorPalette {
    depth: usize,
    colors: [Color; 6],
}

impl ContextColorTheme {
    #[must_use]
    pub fn resolve(self, colorfgbg: Option<&str>) -> ResolvedContextColorTheme {
        match self {
            Self::Dark => ResolvedContextColorTheme::Dark,
            Self::Light => ResolvedContextColorTheme::Light,
            Self::Auto => colorfgbg
                .and_then(|value| value.rsplit(';').next())
                .and_then(|value| value.trim().parse::<u8>().ok())
                .map(xterm_rgb)
                .map(|[r, g, b]| colorgrad::Color::from_rgba8(r, g, b, 255).to_oklaba()[0])
                .filter(|lightness| *lightness >= 0.5)
                .map_or(ResolvedContextColorTheme::Dark, |_| {
                    ResolvedContextColorTheme::Light
                }),
        }
    }
}

impl TerminalColorCapability {
    #[must_use]
    pub fn resolve(colorterm: Option<&str>, term: Option<&str>) -> Self {
        if colorterm.is_some_and(|value| {
            let value = value.trim();
            value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
        }) {
            Self::Truecolor
        } else if term.is_some_and(|value| value.to_ascii_lowercase().contains("256color")) {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }
}

impl ContextColorPalette {
    #[must_use]
    pub fn new(
        depth: usize,
        theme: ResolvedContextColorTheme,
        capability: TerminalColorCapability,
    ) -> Self {
        let colors = match capability {
            TerminalColorCapability::Truecolor => truecolor_colors(theme).map(Color::Rgb),
            TerminalColorCapability::Ansi256 => ansi256_colors(theme).map(Color::Ansi256),
            TerminalColorCapability::Ansi16 => ansi16_colors(theme).map(Color::Ansi),
        };
        Self { depth, colors }
    }

    #[must_use]
    pub fn style(&self, signed_distance: isize) -> ContextStyle {
        let distance = signed_distance.unsigned_abs();
        if distance == 0 || distance > self.depth {
            return ContextStyle::Distant;
        }
        ContextStyle::Proximity(self.colors[(distance - 1) % self.colors.len()])
    }
}

const XTERM_ANSI16: [[u8; 3]; 16] = [
    [0, 0, 0],
    [128, 0, 0],
    [0, 128, 0],
    [128, 128, 0],
    [0, 0, 128],
    [128, 0, 128],
    [0, 128, 128],
    [192, 192, 192],
    [128, 128, 128],
    [255, 0, 0],
    [0, 255, 0],
    [255, 255, 0],
    [0, 0, 255],
    [255, 0, 255],
    [0, 255, 255],
    [255, 255, 255],
];
const XTERM_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn truecolor_colors(theme: ResolvedContextColorTheme) -> [RgbColor; 6] {
    match theme {
        ResolvedContextColorTheme::Dark => [
            RgbColor(0x20, 0xd0, 0xb0),
            RgbColor(0xff, 0xff, 0x30),
            RgbColor(0xff, 0x10, 0xff),
            RgbColor(0xe0, 0xc0, 0x10),
            RgbColor(0x90, 0xff, 0xe0),
            RgbColor(0xb0, 0x90, 0x00),
        ],
        ResolvedContextColorTheme::Light => [
            RgbColor(0xa0, 0x50, 0x90),
            RgbColor(0x10, 0x00, 0x30),
            RgbColor(0xe0, 0x00, 0x10),
            RgbColor(0x20, 0x10, 0x90),
            RgbColor(0x20, 0x60, 0xff),
            RgbColor(0x70, 0x10, 0x10),
        ],
    }
}

fn ansi256_colors(theme: ResolvedContextColorTheme) -> [Ansi256Color; 6] {
    match theme {
        ResolvedContextColorTheme::Dark => [43, 11, 13, 178, 122, 136].map(Ansi256Color),
        ResolvedContextColorTheme::Light => [132, 233, 160, 18, 27, 1].map(Ansi256Color),
    }
}

fn ansi16_colors(theme: ResolvedContextColorTheme) -> [AnsiColor; 6] {
    match theme {
        ResolvedContextColorTheme::Dark => [
            AnsiColor::White,
            AnsiColor::BrightGreen,
            AnsiColor::BrightWhite,
            AnsiColor::BrightMagenta,
            AnsiColor::BrightCyan,
            AnsiColor::BrightYellow,
        ],
        ResolvedContextColorTheme::Light => [
            AnsiColor::Black,
            AnsiColor::Magenta,
            AnsiColor::Green,
            AnsiColor::Blue,
            AnsiColor::Red,
            AnsiColor::BrightBlue,
        ],
    }
}

fn xterm_rgb(index: u8) -> [u8; 3] {
    match index {
        0..=15 => XTERM_ANSI16[usize::from(index)],
        16..=231 => {
            let offset = usize::from(index - 16);
            [
                XTERM_LEVELS[offset / 36],
                XTERM_LEVELS[(offset / 6) % 6],
                XTERM_LEVELS[offset % 6],
            ]
        }
        232..=255 => {
            let value = 8 + 10 * (index - 232);
            [value, value, value]
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorPolicy {
    enabled: bool,
}

impl ColorPolicy {
    #[must_use]
    pub fn resolve(mode: ColorMode, no_color: bool, is_terminal: bool) -> Self {
        let enabled = !no_color
            && match mode {
                ColorMode::Auto => is_terminal,
                ColorMode::Always => true,
                ColorMode::Never => false,
            };
        Self { enabled }
    }

    #[must_use]
    pub fn enabled(self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use anstyle::{Ansi256Color, AnsiColor, Color, RgbColor};

    use super::*;

    type Vector3 = [f64; 3];
    type Matrix3 = [[f64; 3]; 3];

    const IDENTITY: Matrix3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    const PROTANOPIA: Matrix3 = [
        [0.152286, 1.052583, -0.204868],
        [0.114503, 0.786281, 0.099216],
        [-0.003882, -0.048116, 1.051998],
    ];
    const DEUTERANOPIA: Matrix3 = [
        [0.367322, 0.860646, -0.227968],
        [0.280085, 0.672501, 0.047413],
        [-0.011820, 0.042940, 0.968881],
    ];
    const TRITANOPIA: Matrix3 = [
        [1.255528, -0.076749, -0.178779],
        [-0.078411, 0.930809, 0.147602],
        [0.004733, 0.691367, 0.303900],
    ];

    const DARK: [RgbColor; 6] = [
        RgbColor(0x20, 0xd0, 0xb0),
        RgbColor(0xff, 0xff, 0x30),
        RgbColor(0xff, 0x10, 0xff),
        RgbColor(0xe0, 0xc0, 0x10),
        RgbColor(0x90, 0xff, 0xe0),
        RgbColor(0xb0, 0x90, 0x00),
    ];
    const LIGHT: [RgbColor; 6] = [
        RgbColor(0xa0, 0x50, 0x90),
        RgbColor(0x10, 0x00, 0x30),
        RgbColor(0xe0, 0x00, 0x10),
        RgbColor(0x20, 0x10, 0x90),
        RgbColor(0x20, 0x60, 0xff),
        RgbColor(0x70, 0x10, 0x10),
    ];

    fn proximity_color(palette: &ContextColorPalette, distance: isize) -> Color {
        match palette.style(distance) {
            ContextStyle::Proximity(color) => color,
            ContextStyle::Distant => panic!("expected a proximity color at {distance}"),
        }
    }

    #[test]
    fn discrete_palette_is_symmetric_stable_and_cycles_inside_depth() {
        use ResolvedContextColorTheme::Dark;
        use TerminalColorCapability::Truecolor;

        let five = ContextColorPalette::new(5, Dark, Truecolor);
        let twenty = ContextColorPalette::new(20, Dark, Truecolor);
        for distance in 1..=5 {
            assert_eq!(
                proximity_color(&five, distance),
                proximity_color(&twenty, distance)
            );
            assert_eq!(
                proximity_color(&twenty, -distance),
                proximity_color(&twenty, distance)
            );
        }
        let cycle = (1..=6)
            .map(|distance| proximity_color(&twenty, distance))
            .collect::<Vec<_>>();
        for (index, color) in cycle.iter().enumerate() {
            assert!(!cycle[..index].contains(color));
        }
        assert_eq!(proximity_color(&twenty, 7), cycle[0]);
        assert_eq!(proximity_color(&twenty, 9), cycle[2]);
        for distance in 1..20 {
            assert_ne!(
                proximity_color(&twenty, distance),
                proximity_color(&twenty, distance + 1)
            );
        }
        assert_eq!(twenty.style(0), ContextStyle::Distant);
        assert_eq!(twenty.style(21), ContextStyle::Distant);
    }

    #[test]
    fn capabilities_select_the_frozen_six_color_palettes() {
        use AnsiColor::*;
        use ResolvedContextColorTheme::*;
        use TerminalColorCapability::*;

        assert_eq!(
            ContextColorPalette::new(20, Dark, Truecolor).colors,
            DARK.map(Color::Rgb)
        );
        assert_eq!(
            ContextColorPalette::new(20, Light, Truecolor).colors,
            LIGHT.map(Color::Rgb)
        );
        assert_eq!(
            ContextColorPalette::new(20, Dark, Ansi256).colors,
            [43, 11, 13, 178, 122, 136].map(|index| Color::Ansi256(Ansi256Color(index)))
        );
        assert_eq!(
            ContextColorPalette::new(20, Light, Ansi256).colors,
            [132, 233, 160, 18, 27, 1].map(|index| Color::Ansi256(Ansi256Color(index)))
        );
        assert_eq!(
            ContextColorPalette::new(20, Dark, Ansi16).colors,
            [
                White,
                BrightGreen,
                BrightWhite,
                BrightMagenta,
                BrightCyan,
                BrightYellow
            ]
            .map(Color::Ansi)
        );
        assert_eq!(
            ContextColorPalette::new(20, Light, Ansi16).colors,
            [Black, Magenta, Green, Blue, Red, BrightBlue].map(Color::Ansi)
        );
    }

    #[test]
    fn frozen_palettes_preserve_recorded_cvd_distances_and_contrast() {
        assert_distance_floors(
            minimum_pair_distances(DARK),
            [0.150751, 0.138593, 0.142038, 0.137228],
        );
        assert_distance_floors(
            minimum_cycle_distances(DARK),
            [0.209932, 0.200474, 0.164225, 0.202215],
        );
        assert_distance_floors(
            minimum_pair_distances(LIGHT),
            [0.186347, 0.161430, 0.173097, 0.167791],
        );
        assert_distance_floors(
            minimum_cycle_distances(LIGHT),
            [0.228540, 0.242462, 0.219237, 0.190787],
        );
        assert_minimum_contrast(
            &DARK.map(Color::Rgb),
            ["#000000", "#1E1E1E", "#282A36"],
            4.5,
        );
        assert_minimum_contrast(
            &LIGHT.map(Color::Rgb),
            ["#FFFFFF", "#FDF6E3", "#F5F5F5"],
            4.5,
        );
    }

    #[test]
    fn theme_and_capability_resolvers_are_pure_and_conservative() {
        assert_eq!(
            ContextColorTheme::Auto.resolve(None),
            ResolvedContextColorTheme::Dark
        );
        assert_eq!(
            ContextColorTheme::Auto.resolve(Some("15;0")),
            ResolvedContextColorTheme::Dark
        );
        assert_eq!(
            ContextColorTheme::Auto.resolve(Some("0;15")),
            ResolvedContextColorTheme::Light
        );
        assert_eq!(
            ContextColorTheme::Auto.resolve(Some("broken")),
            ResolvedContextColorTheme::Dark
        );
        assert_eq!(
            ContextColorTheme::Dark.resolve(Some("0;15")),
            ResolvedContextColorTheme::Dark
        );
        assert_eq!(
            ContextColorTheme::Light.resolve(Some("15;0")),
            ResolvedContextColorTheme::Light
        );
        assert_eq!(
            TerminalColorCapability::resolve(Some(" truecolor "), None),
            TerminalColorCapability::Truecolor
        );
        assert_eq!(
            TerminalColorCapability::resolve(Some("24BIT"), None),
            TerminalColorCapability::Truecolor
        );
        assert_eq!(
            TerminalColorCapability::resolve(None, Some("xterm-256color")),
            TerminalColorCapability::Ansi256
        );
        assert_eq!(
            TerminalColorCapability::resolve(None, Some("xterm")),
            TerminalColorCapability::Ansi16
        );
    }

    fn transform(matrix: Matrix3, value: Vector3) -> Vector3 {
        matrix.map(|row| {
            row.into_iter()
                .zip(value)
                .map(|(coefficient, channel)| coefficient * channel)
                .sum::<f64>()
                .clamp(0.0, 1.0)
        })
    }

    fn oklab_from_linear([red, green, blue]: Vector3) -> Vector3 {
        let long_response = 0.4122214708 * red + 0.5363325363 * green + 0.0514459929 * blue;
        let medium_response = 0.2119034982 * red + 0.6806995451 * green + 0.1073969566 * blue;
        let short_response = 0.0883024619 * red + 0.2817188376 * green + 0.6299787005 * blue;
        let [long_response, medium_response, short_response] = [
            long_response.cbrt(),
            medium_response.cbrt(),
            short_response.cbrt(),
        ];
        [
            0.2104542553 * long_response + 0.7936177850 * medium_response
                - 0.0040720468 * short_response,
            1.9779984951 * long_response - 2.4285922050 * medium_response
                + 0.4505937099 * short_response,
            0.0259040371 * long_response + 0.7827717662 * medium_response
                - 0.8086757660 * short_response,
        ]
    }

    fn vision_labs(RgbColor(r, g, b): RgbColor) -> [Vector3; 4] {
        let linear = [r, g, b].map(|channel| {
            let channel = f64::from(channel) / 255.0;
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        });
        [IDENTITY, PROTANOPIA, DEUTERANOPIA, TRITANOPIA]
            .map(|model| oklab_from_linear(transform(model, linear)))
    }

    fn distance(first: Vector3, second: Vector3) -> f64 {
        first
            .into_iter()
            .zip(second)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f64>()
            .sqrt()
    }

    fn minimum_pair_distances(colors: [RgbColor; 6]) -> [f64; 4] {
        let labs = colors.map(vision_labs);
        let mut minima = [f64::INFINITY; 4];
        for first in 0..labs.len() {
            for second in first + 1..labs.len() {
                for model in 0..minima.len() {
                    minima[model] =
                        minima[model].min(distance(labs[first][model], labs[second][model]));
                }
            }
        }
        minima
    }

    fn minimum_cycle_distances(colors: [RgbColor; 6]) -> [f64; 4] {
        let labs = colors.map(vision_labs);
        let mut minima = [f64::INFINITY; 4];
        for first in 0..labs.len() {
            let second = (first + 1) % labs.len();
            for model in 0..minima.len() {
                minima[model] =
                    minima[model].min(distance(labs[first][model], labs[second][model]));
            }
        }
        minima
    }

    fn assert_distance_floors(actual: [f64; 4], floors: [f64; 4]) {
        for (actual, floor) in actual.into_iter().zip(floors) {
            assert!(
                actual + 0.000_001 >= floor,
                "Oklab distance {actual} is below recorded floor {floor}"
            );
        }
    }

    fn assert_minimum_contrast(colors: &[Color], backgrounds: [&str; 3], minimum: f32) {
        for color in colors {
            let foreground = color_rgb(*color);
            for background in backgrounds {
                let ratio = contrast_ratio(foreground, parse_hex(background));
                assert!(ratio >= minimum, "contrast {ratio} is below {minimum}");
            }
        }
    }

    fn color_rgb(color: Color) -> [u8; 3] {
        match color {
            Color::Rgb(RgbColor(r, g, b)) => [r, g, b],
            Color::Ansi256(index) => xterm_rgb(index.0),
            Color::Ansi(_) => panic!("test expected an RGB-addressable color"),
        }
    }

    fn parse_hex(value: &str) -> [u8; 3] {
        [
            u8::from_str_radix(value.get(1..3).unwrap(), 16).unwrap(),
            u8::from_str_radix(value.get(3..5).unwrap(), 16).unwrap(),
            u8::from_str_radix(value.get(5..7).unwrap(), 16).unwrap(),
        ]
    }

    fn contrast_ratio(first: [u8; 3], second: [u8; 3]) -> f32 {
        let first = relative_luminance(first);
        let second = relative_luminance(second);
        (first.max(second) + 0.05) / (first.min(second) + 0.05)
    }

    fn relative_luminance([r, g, b]: [u8; 3]) -> f32 {
        let linear = [r, g, b].map(|channel| {
            let channel = f32::from(channel) / 255.0;
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        });
        0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]
    }
}
