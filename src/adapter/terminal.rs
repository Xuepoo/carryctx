use std::io::{self, BufRead, IsTerminal, Write};

use crate::error::CarryCtxError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalAnswer<T> {
    Value(T),
    Cancel,
    Eof,
}

pub trait Terminal {
    fn stdin_is_tty(&self) -> bool;
    fn text(&self, message: &str) -> Result<TerminalAnswer<String>, CarryCtxError>;
    fn confirm(
        &self,
        message: &str,
        initial_value: bool,
    ) -> Result<TerminalAnswer<bool>, CarryCtxError>;
}

pub struct ProcessTerminal;

impl ProcessTerminal {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl Terminal for ProcessTerminal {
    fn stdin_is_tty(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn text(&self, message: &str) -> Result<TerminalAnswer<String>, CarryCtxError> {
        let mut stdout = io::stdout();
        write!(stdout, "{}: ", message).map_err(|_| CarryCtxError::interrupted())?;
        stdout.flush().map_err(|_| CarryCtxError::interrupted())?;

        let mut input = String::new();
        match io::stdin().lock().read_line(&mut input) {
            Ok(0) => Ok(TerminalAnswer::Eof),
            Ok(_) => {
                let trimmed = input.trim().to_string();
                if trimmed.is_empty() {
                    Ok(TerminalAnswer::Cancel)
                } else {
                    Ok(TerminalAnswer::Value(trimmed))
                }
            }
            Err(_) => Ok(TerminalAnswer::Cancel),
        }
    }

    fn confirm(
        &self,
        message: &str,
        initial_value: bool,
    ) -> Result<TerminalAnswer<bool>, CarryCtxError> {
        let default_str = if initial_value { "Y/n" } else { "y/N" };
        let mut stdout = io::stdout();
        write!(stdout, "{} [{}]: ", message, default_str)
            .map_err(|_| CarryCtxError::interrupted())?;
        stdout.flush().map_err(|_| CarryCtxError::interrupted())?;

        let mut input = String::new();
        match io::stdin().lock().read_line(&mut input) {
            Ok(0) => Ok(TerminalAnswer::Eof),
            Ok(_) => {
                let trimmed = input.trim().to_lowercase();
                match trimmed.as_str() {
                    "" => Ok(TerminalAnswer::Value(initial_value)),
                    "y" | "yes" | "true" => Ok(TerminalAnswer::Value(true)),
                    "n" | "no" | "false" => Ok(TerminalAnswer::Value(false)),
                    "c" => Ok(TerminalAnswer::Cancel),
                    _ => Ok(TerminalAnswer::Value(initial_value)),
                }
            }
            Err(_) => Ok(TerminalAnswer::Cancel),
        }
    }
}

pub struct FakeTerminal {
    pub stdin_is_tty: bool,
    pub text_responses: Vec<TerminalAnswer<String>>,
    pub confirm_responses: Vec<TerminalAnswer<bool>>,
}

impl Default for FakeTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeTerminal {
    pub fn new() -> Self {
        Self {
            stdin_is_tty: true,
            text_responses: vec![],
            confirm_responses: vec![],
        }
    }

    pub fn with_text(mut self, responses: Vec<TerminalAnswer<String>>) -> Self {
        self.text_responses = responses;
        self
    }

    pub fn with_confirm(mut self, responses: Vec<TerminalAnswer<bool>>) -> Self {
        self.confirm_responses = responses;
        self
    }
}

impl Terminal for FakeTerminal {
    fn stdin_is_tty(&self) -> bool {
        self.stdin_is_tty
    }

    fn text(&self, _message: &str) -> Result<TerminalAnswer<String>, CarryCtxError> {
        if let Some(response) = self.text_responses.first().cloned() {
            Ok(response)
        } else {
            Ok(TerminalAnswer::Cancel)
        }
    }

    fn confirm(
        &self,
        _message: &str,
        _initial_value: bool,
    ) -> Result<TerminalAnswer<bool>, CarryCtxError> {
        if let Some(response) = self.confirm_responses.first().cloned() {
            Ok(response)
        } else {
            Ok(TerminalAnswer::Cancel)
        }
    }
}
