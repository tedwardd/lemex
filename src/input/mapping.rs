use std::collections::HashMap;

use crossterm::event::KeyCode;

use super::command::Command;

pub trait IntoKeySequence {
    fn into_key_sequence(self) -> Vec<KeyCode>;
}

impl IntoKeySequence for &str {
    fn into_key_sequence(self) -> Vec<KeyCode> {
        self.chars().map(KeyCode::Char).collect()
    }
}

impl IntoKeySequence for String {
    fn into_key_sequence(self) -> Vec<KeyCode> {
        self.chars().map(KeyCode::Char).collect()
    }
}

impl IntoKeySequence for char {
    fn into_key_sequence(self) -> Vec<KeyCode> {
        vec![KeyCode::Char(self)]
    }
}

impl IntoKeySequence for Vec<char> {
    fn into_key_sequence(self) -> Vec<KeyCode> {
        self.into_iter().map(KeyCode::Char).collect()
    }
}

impl IntoKeySequence for Vec<KeyCode> {
    fn into_key_sequence(self) -> Vec<KeyCode> {
        self
    }
}

impl<'a> IntoKeySequence for &'a [KeyCode] {
    fn into_key_sequence(self) -> Vec<KeyCode> {
        self.to_vec()
    }
}

impl<'a> IntoKeySequence for &'a [char] {
    fn into_key_sequence(self) -> Vec<KeyCode> {
        self.iter().copied().map(KeyCode::Char).collect()
    }
}

impl<const N: usize> IntoKeySequence for [KeyCode; N] {
    fn into_key_sequence(self) -> Vec<KeyCode> {
        self.into_iter().collect()
    }
}

impl IntoKeySequence for KeyCode {
    fn into_key_sequence(self) -> Vec<KeyCode> {
        vec![self]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingMatch {
    Complete(Command),
    Prefix,
    NoMatch,
}

#[derive(Debug, Clone, Default)]
pub struct MappingTable {
    mappings: HashMap<Vec<KeyCode>, Command>,
}

impl MappingTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert<S>(&mut self, sequence: S, command: Command)
    where
        S: IntoKeySequence,
    {
        let sequence = sequence.into_key_sequence();
        if !sequence.is_empty() {
            self.mappings.insert(sequence, command);
        }
    }

    /// Remove every sequence currently bound to `command`, so a rebinding
    /// replaces (rather than shadows) the previous binding.
    pub fn remove_command(&mut self, command: &Command) {
        self.mappings.retain(|_, bound| bound != command);
    }

    pub fn resolve<S>(&self, sequence: S) -> Option<Command>
    where
        S: IntoKeySequence,
    {
        let sequence = sequence.into_key_sequence();
        self.longest_complete(sequence.as_slice()).map(|(_, command)| command)
    }

    pub fn longest_complete<S>(&self, sequence: S) -> Option<(usize, Command)>
    where
        S: IntoKeySequence,
    {
        let sequence = sequence.into_key_sequence();
        self.mappings
            .iter()
            .filter(|(mapping, _)| sequence.starts_with(mapping.as_slice()))
            .max_by_key(|(mapping, _)| mapping.len())
            .map(|(mapping, command)| (mapping.len(), command.clone()))
    }

    pub fn has_prefix<S>(&self, sequence: S) -> bool
    where
        S: IntoKeySequence,
    {
        let sequence = sequence.into_key_sequence();
        self.mappings
            .keys()
            .any(|mapping| mapping.starts_with(sequence.as_slice()) && mapping.len() > sequence.len())
    }

    pub fn classify<S>(&self, sequence: S) -> MappingMatch
    where
        S: IntoKeySequence,
    {
        let sequence = sequence.into_key_sequence();
        if let Some((_, command)) = self.longest_complete(sequence.as_slice()) {
            MappingMatch::Complete(command)
        } else if self.has_prefix(sequence.as_slice()) {
            MappingMatch::Prefix
        } else {
            MappingMatch::NoMatch
        }
    }
}
