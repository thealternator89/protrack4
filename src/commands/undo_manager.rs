use std::collections::VecDeque;

use rusqlite::{Connection, TransactionBehavior};

use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::map_sqlite_err;
use crate::domain::DomainError;

pub const MAX_UNDO_DEPTH: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoRedoStatus {
    pub can_undo: bool,
    pub can_redo: bool,
    pub undo_label: Option<String>,
    pub redo_label: Option<String>,
    pub undo_len: usize,
    pub redo_len: usize,
}

pub struct UndoManager {
    undo_stack: VecDeque<Box<dyn UndoableCommand>>,
    redo_stack: Vec<Box<dyn UndoableCommand>>,
    max_depth: usize,
}

impl Default for UndoManager {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoManager {
    pub fn new() -> Self {
        Self::with_capacity(MAX_UNDO_DEPTH)
    }

    pub fn with_capacity(max_depth: usize) -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
            max_depth: max_depth.max(1),
        }
    }

    pub fn status(&self) -> UndoRedoStatus {
        UndoRedoStatus {
            can_undo: !self.undo_stack.is_empty(),
            can_redo: !self.redo_stack.is_empty(),
            undo_label: self.undo_stack.back().map(|c| c.label()),
            redo_label: self.redo_stack.last().map(|c| c.label()),
            undo_len: self.undo_stack.len(),
            redo_len: self.redo_stack.len(),
        }
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    pub fn execute(
        &mut self,
        conn: &mut Connection,
        mut command: Box<dyn UndoableCommand>,
    ) -> Result<CommandEffect, DomainError> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let effect = command.execute(&tx)?;
        tx.commit().map_err(map_sqlite_err)?;

        if command.clears_undo_history() {
            self.clear();
        } else {
            self.redo_stack.clear();
            self.undo_stack.push_back(command);
            while self.undo_stack.len() > self.max_depth {
                self.undo_stack.pop_front();
            }
        }

        Ok(effect)
    }

    pub fn undo(&mut self, conn: &mut Connection) -> Result<Option<CommandEffect>, DomainError> {
        let Some(mut cmd) = self.undo_stack.pop_back() else {
            return Ok(None);
        };

        let tx = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
            Ok(tx) => tx,
            Err(err) => {
                self.undo_stack.push_back(cmd);
                return Err(map_sqlite_err(err));
            }
        };

        let mut effect = match cmd.undo(&tx) {
            Ok(effect) => effect,
            Err(err) => {
                drop(tx);
                self.undo_stack.push_back(cmd);
                return Err(err);
            }
        };

        if let Err(err) = tx.commit() {
            self.undo_stack.push_back(cmd);
            return Err(map_sqlite_err(err));
        }

        effect.toast = Some(format!("Undid: {}", cmd.label()));
        self.redo_stack.push(cmd);
        Ok(Some(effect))
    }

    pub fn redo(&mut self, conn: &mut Connection) -> Result<Option<CommandEffect>, DomainError> {
        let Some(mut cmd) = self.redo_stack.pop() else {
            return Ok(None);
        };

        let tx = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
            Ok(tx) => tx,
            Err(err) => {
                self.redo_stack.push(cmd);
                return Err(map_sqlite_err(err));
            }
        };

        let mut effect = match cmd.execute(&tx) {
            Ok(effect) => effect,
            Err(err) => {
                drop(tx);
                self.redo_stack.push(cmd);
                return Err(err);
            }
        };

        if let Err(err) = tx.commit() {
            self.redo_stack.push(cmd);
            return Err(map_sqlite_err(err));
        }

        if cmd.clears_undo_history() {
            self.clear();
        } else {
            effect.toast = Some(format!("Redid: {}", cmd.label()));
            self.undo_stack.push_back(cmd);
            while self.undo_stack.len() > self.max_depth {
                self.undo_stack.pop_front();
            }
        }

        Ok(Some(effect))
    }
}
