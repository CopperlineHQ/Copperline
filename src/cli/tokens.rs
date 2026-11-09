// SPDX-License-Identifier: GPL-3.0-or-later

//! Option boundaries are separate from value consumption: only an option
//! token may split at `=`, never a path, script text, or guest argument.

use anyhow::{bail, Result};
use std::collections::VecDeque;

pub(super) struct Arguments {
    // The bool marks script-origin tokens, including values that could
    // otherwise become an unrestricted option after a malformed directive.
    remaining: VecDeque<(String, bool)>,
    inline_value: Option<String>,
}

impl Arguments {
    pub(super) fn new(args: impl IntoIterator<Item = String>) -> Self {
        Self {
            remaining: args.into_iter().map(|arg| (arg, false)).collect(),
            inline_value: None,
        }
    }

    pub(super) fn next_option(&mut self) -> Result<Option<String>> {
        debug_assert!(self.inline_value.is_none());
        let Some((arg, script)) = self.remaining.pop_front() else {
            return Ok(None);
        };
        let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
        if script && !super::SCRIPT_DIRECTIVES.contains(&flag.trim_start_matches("--")) {
            bail!("{flag:?} is not a scripted-input directive");
        }
        if arg.starts_with("--") {
            if let Some((flag, value)) = arg.split_once('=') {
                self.inline_value = Some(value.to_owned());
                return Ok(Some(flag.to_owned()));
            }
        }
        Ok(Some(arg))
    }

    pub(super) fn finish_option(&self, flag: &str) -> Result<()> {
        if self.inline_value.is_some() {
            bail!("{flag} does not take a value");
        }
        Ok(())
    }

    pub(super) fn peek(&self) -> Option<&String> {
        self.inline_value
            .as_ref()
            .or_else(|| self.remaining.front().map(|(arg, _)| arg))
    }

    pub(super) fn prepend(&mut self, args: Vec<String>) {
        for arg in args.into_iter().rev() {
            self.remaining.push_front((arg, true));
        }
    }
}

impl Iterator for Arguments {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        self.inline_value
            .take()
            .or_else(|| self.remaining.pop_front().map(|(arg, _)| arg))
    }
}
