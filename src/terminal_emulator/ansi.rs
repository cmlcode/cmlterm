#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectGraphicRendition {
    // NOTE: Non-exhaustive list
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Unknown(usize),
}

impl SelectGraphicRendition {
    fn from_usize(val: usize) -> SelectGraphicRendition {
        match val {
            0 => SelectGraphicRendition::Reset,
            30 => SelectGraphicRendition::Black,
            31 => SelectGraphicRendition::Red,
            32 => SelectGraphicRendition::Green,
            33 => SelectGraphicRendition::Yellow,
            34 => SelectGraphicRendition::Blue,
            35 => SelectGraphicRendition::Magenta,
            36 => SelectGraphicRendition::Cyan,
            37 => SelectGraphicRendition::White,
            90 => SelectGraphicRendition::BrightBlack,
            91 => SelectGraphicRendition::BrightRed,
            92 => SelectGraphicRendition::BrightGreen,
            93 => SelectGraphicRendition::BrightYellow,
            94 => SelectGraphicRendition::BrightBlue,
            95 => SelectGraphicRendition::BrightMagenta,
            96 => SelectGraphicRendition::BrightCyan,
            97 => SelectGraphicRendition::BrightWhite,
            _ => Self::Unknown(val),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum TerminalOutput {
    SetCursorPos { x: Option<usize>, y: Option<usize> },
    ClearForwards,
    ClearBackwards,
    ClearAll,
    Sgr(SelectGraphicRendition),
    Data(Vec<u8>),
    Invalid,
}

enum CsiParserState {
    Params,
    Intermediates,
    Finished(u8),
    Invalid,
    InvalidFinished,
}

fn is_csi_terminator(b: u8) -> bool {
    (0x40..=0x7d).contains(&b)
}

fn is_csi_param(b: u8) -> bool {
    (0x30..=0x3f).contains(&b)
}

fn is_csi_intermediate(b: u8) -> bool {
    (0x20..=0x2f).contains(&b)
}

fn extract_param(idx: usize, params: &[Option<usize>]) -> Option<usize> {
    params.get(idx).copied().flatten()
}

fn split_params_into_semicolon_delimited_usize(params: &[u8]) -> Result<Vec<Option<usize>>, ()> {
    let params = params
        .split(|b| *b == b';')
        .map(parse_param_as_usize)
        .collect::<Result<Vec<Option<usize>>, ()>>();

    params
}

fn parse_param_as_usize(param_bytes: &[u8]) -> Result<Option<usize>, ()> {
    let param_str =
        std::str::from_utf8(param_bytes).expect("parameter should always be valid utf8");
    if param_str.is_empty() {
        return Ok(None);
    }
    let param = param_str.parse().map_err(|_| ())?;
    Ok(Some(param))
}

struct CsiParser {
    state: CsiParserState,
    params: Vec<u8>,
    intermediates: Vec<u8>,
}

impl CsiParser {
    fn new() -> CsiParser {
        CsiParser {
            state: CsiParserState::Params,
            params: Vec::new(),
            intermediates: Vec::new(),
        }
    }

    fn push(&mut self, b: u8) {
        if let CsiParserState::Finished(_) | CsiParserState::InvalidFinished = &self.state {
            panic!("CsiParser should not be pushed to once finished");
        }

        match &mut self.state {
            CsiParserState::Params => {
                if is_csi_param(b) {
                    self.params.push(b);
                } else if is_csi_intermediate(b) {
                    self.intermediates.push(b);
                    self.state = CsiParserState::Intermediates;
                } else if is_csi_terminator(b) {
                    self.state = CsiParserState::Finished(b);
                } else {
                    self.state = CsiParserState::Invalid
                }
            }
            CsiParserState::Intermediates => {
                if is_csi_param(b) {
                    self.state = CsiParserState::Invalid;
                } else if is_csi_intermediate(b) {
                    self.intermediates.push(b);
                } else if is_csi_terminator(b) {
                    self.state = CsiParserState::Finished(b);
                } else {
                    self.state = CsiParserState::Invalid
                }
            }
            CsiParserState::Invalid => {
                if is_csi_terminator(b) {
                    self.state = CsiParserState::InvalidFinished;
                }
            }
            CsiParserState::Finished(_) | CsiParserState::InvalidFinished => {
                unreachable!();
            }
        }
    }
}

enum AnsiParserInner {
    Empty,
    Escape,
    Csi(CsiParser),
}

pub struct AnsiParser {
    inner: AnsiParserInner,
}

impl AnsiParser {
    pub fn new() -> AnsiParser {
        AnsiParser {
            inner: AnsiParserInner::Empty,
        }
    }

    pub fn push(&mut self, incoming: &[u8]) -> Vec<TerminalOutput> {
        let mut output = Vec::new();
        let mut data_output = Vec::new();
        for b in incoming {
            match &mut self.inner {
                AnsiParserInner::Empty => {
                    if *b == b'\x1b' {
                        self.inner = AnsiParserInner::Escape;
                        continue;
                    }

                    data_output.push(*b);
                }
                AnsiParserInner::Escape => {
                    if !data_output.is_empty() {
                        output.push(TerminalOutput::Data(std::mem::take(&mut data_output)));
                    }

                    match b {
                        b'[' => {
                            self.inner = AnsiParserInner::Csi(CsiParser::new());
                        }
                        _ => {
                            let b_utf8 = std::char::from_u32(*b as u32);
                            println!("Unhandled escape sequence {b_utf8:?} {b:x}");
                            self.inner = AnsiParserInner::Empty;
                        }
                    }
                }
                AnsiParserInner::Csi(parser) => {
                    parser.push(*b);
                    match parser.state {
                        CsiParserState::Finished(b'H') => {
                            let params =
                                split_params_into_semicolon_delimited_usize(&parser.params);

                            let Ok(params) = params else {
                                println!("Invalid cursor set position sequence");
                                output.push(TerminalOutput::Invalid);
                                self.inner = AnsiParserInner::Empty;
                                continue;
                            };

                            output.push(TerminalOutput::SetCursorPos {
                                x: Some(extract_param(0, &params).unwrap_or(1)),
                                y: Some(extract_param(1, &params).unwrap_or(1)),
                            });
                            self.inner = AnsiParserInner::Empty;
                        }
                        CsiParserState::Finished(b'G') => {
                            let Ok(param) = parse_param_as_usize(&parser.params) else {
                                println!("Invalid cursor set position sequence");
                                output.push(TerminalOutput::Invalid);
                                self.inner = AnsiParserInner::Empty;
                                continue;
                            };

                            let x_pos = param.unwrap_or(1);

                            output.push(TerminalOutput::SetCursorPos {
                                x: Some(x_pos),
                                y: None,
                            });
                            self.inner = AnsiParserInner::Empty;
                        }
                        CsiParserState::Finished(b'J') => {
                            let Ok(param) = parse_param_as_usize(&parser.params) else {
                                println!("Invalid clear command");
                                output.push(TerminalOutput::Invalid);
                                self.inner = AnsiParserInner::Empty;
                                continue;
                            };

                            let ret = match param.unwrap_or(0) {
                                0 => TerminalOutput::ClearForwards,
                                1 => TerminalOutput::ClearBackwards,
                                2 | 3 => TerminalOutput::ClearAll,
                                _ => TerminalOutput::Invalid,
                            };
                            output.push(ret);
                            self.inner = AnsiParserInner::Empty;
                        }
                        CsiParserState::Finished(b'm') => {
                            let params =
                                split_params_into_semicolon_delimited_usize(&parser.params);

                            let Ok(mut params) = params else {
                                println!("Invalid SGR sequence");
                                output.push(TerminalOutput::Invalid);
                                self.inner = AnsiParserInner::Empty;
                                continue;
                            };

                            if params.is_empty() {
                                params.push(Some(0));
                            }

                            if params.len() == 1 && params[0].is_none() {
                                params[0] = Some(0);
                            }

                            for param in params {
                                let Some(param) = param else {
                                    continue;
                                };
                                output.push(TerminalOutput::Sgr(
                                    SelectGraphicRendition::from_usize(param),
                                ));
                            }

                            self.inner = AnsiParserInner::Empty;
                        }
                        CsiParserState::Finished(esc) => {
                            println!(
                                "Unhandled csi code: {:?} {esc:x}",
                                std::char::from_u32(esc as u32)
                            );
                            output.push(TerminalOutput::Invalid);
                            self.inner = AnsiParserInner::Empty;
                        }
                        CsiParserState::Invalid => {
                            println!("Invalid CSI sequence");
                            output.push(TerminalOutput::Invalid);
                            self.inner = AnsiParserInner::Empty;
                        }
                        _ => {}
                    }
                }
            }
        }

        if !data_output.is_empty() {
            output.push(TerminalOutput::Data(data_output));
        }

        output
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_set_cursor_position() {
        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[32;15H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos {
                x: Some(32),
                y: Some(15)
            }
        ));

        let parsed = output_buffer.push(b"\x1b[;32H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos {
                x: Some(1),
                y: Some(32)
            }
        ));

        let parsed = output_buffer.push(b"\x1b[32H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos {
                x: Some(32),
                y: Some(1)
            }
        ));

        let parsed = output_buffer.push(b"\x1b[32;H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos {
                x: Some(32),
                y: Some(1)
            }
        ));

        let parsed = output_buffer.push(b"\x1b[H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos {
                x: Some(1),
                y: Some(1)
            }
        ));

        let parsed = output_buffer.push(b"\x1b[;H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos {
                x: Some(1),
                y: Some(1)
            }
        ));
    }

    #[test]
    fn test_clear() {
        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::ClearForwards,));

        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[0J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::ClearForwards,));

        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[1J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::ClearBackwards,));

        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[2J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::ClearAll,));
    }

    #[test]
    fn test_invalid_clear() {
        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[8J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::Invalid,));
    }

    #[test]
    fn test_invalid_csi() {
        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[-23;H");
        assert!(matches!(parsed[0], TerminalOutput::Invalid));

        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[asdf");
        assert!(matches!(parsed[0], TerminalOutput::Invalid));
    }

    #[test]
    fn test_parsing_unknown_csi() {
        let mut parser = CsiParser::new();
        for b in b"0123456789:;<=>?!\"#$%&'()*+,-./}" {
            parser.push(*b);
        }

        assert_eq!(parser.params, b"0123456789:;<=>?");
        assert_eq!(parser.intermediates, b"!\"#$%&'()*+,-./");
        assert!(matches!(parser.state, CsiParserState::Finished(b'}')));
    }

    #[test]
    fn test_parsing_invalid_csi() {
        let mut parser = CsiParser::new();
        for b in b"0$0" {
            parser.push(*b);
        }

        assert!(matches!(parser.state, CsiParserState::Invalid));
        parser.push(b'm');
        assert!(matches!(parser.state, CsiParserState::InvalidFinished));
    }

    #[test]
    fn test_empty_sgr() {
        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[m");
        assert!(matches!(
            parsed[0],
            TerminalOutput::Sgr(SelectGraphicRendition::Reset)
        ));
    }

    #[test]
    fn test_color_parsing() {
        let mut output_buffer = AnsiParser::new();

        struct ColorCode(u8);

        impl std::fmt::Display for ColorCode {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_fmt(format_args!("\x1b[{}m", self.0))
            }
        }

        let mut test_input = String::new();
        for i in 30..=37 {
            test_input.push_str(&ColorCode(i).to_string());
            test_input.push('a');
        }

        for i in 90..=97 {
            test_input.push_str(&ColorCode(i).to_string());
            test_input.push('a');
        }

        let output = output_buffer.push(test_input.as_bytes());
        assert_eq!(
            output,
            &[
                TerminalOutput::Sgr(SelectGraphicRendition::Black),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::Red),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::Green),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::Yellow),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::Blue),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::Magenta),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::Cyan),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::White),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::BrightBlack),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::BrightRed),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::BrightGreen),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::BrightYellow),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::BrightBlue),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::BrightMagenta),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::BrightCyan),
                TerminalOutput::Data(b"a".into()),
                TerminalOutput::Sgr(SelectGraphicRendition::BrightWhite),
                TerminalOutput::Data(b"a".into()),
            ]
        );
    }
}
