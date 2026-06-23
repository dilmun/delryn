//! The uniform yes/no confirmation for destructive actions. Intercepts input
//! ahead of every popup; answered with `y`/`⏎` (commit) or `n`/`Esc` (cancel).

use crossterm::event::{KeyCode, KeyEvent};

use super::App;

/// A destructive action waiting for a yes/no answer. One uniform prompt across
/// the app, shown in the status bar.
pub struct PendingConfirm {
    /// The question, e.g. `Delete "SciFi"?` or `Rename 3 books?`.
    pub question: String,
    /// What to run when the user confirms.
    action: ConfirmAction,
}

/// The action a [`PendingConfirm`] commits on `yes`. The relevant popup state
/// (editor / rename / collection editor) is still open behind the prompt.
pub(crate) enum ConfirmAction {
    /// Save the metadata editor (fields + embed cover).
    SaveMeta,
    /// Apply the rename template to the popup's targets.
    Rename,
    /// Commit the inline collection editor (rename, or delete on a cleared name).
    Collection,
}

impl App {
    /// Raise the uniform yes/no confirmation for a destructive action. The
    /// underlying popup (editor / rename / collection editor) stays open behind
    /// the prompt, so cancelling returns the user exactly where they were.
    pub(crate) fn ask_confirm(&mut self, question: &str, action: ConfirmAction) {
        self.pending_confirm = Some(PendingConfirm {
            question: question.to_string(),
            action,
        });
    }

    /// Answer the pending confirmation: `y`/`⏎` commits, `n`/`Esc` cancels, and
    /// any other key is ignored (the prompt stays up).
    pub(crate) fn confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.confirm_commit(),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => self.pending_confirm = None,
            _ => {}
        }
    }

    /// Run the confirmed action and dismiss the prompt.
    fn confirm_commit(&mut self) {
        let Some(p) = self.pending_confirm.take() else {
            return;
        };
        match p.action {
            ConfirmAction::SaveMeta => self.save_meta_edit(),
            ConfirmAction::Rename => self.apply_bulk_rename(),
            ConfirmAction::Collection => self.lib_coll_commit(),
        }
    }
}
