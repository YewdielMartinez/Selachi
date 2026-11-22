//! Pure pieces of "edit, then save later": the debounce/single-in-flight gate and the three-way
//! merge that lets a hot reload land without clobbering an edit in progress.

use std::time::Duration;

use store::Config;
use toml::Value;

/// Quiet period after the last edit before the config is written (400 ms).
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// Debounce + single-in-flight state machine. No clock: the caller arms a timer after every
/// [`SaveGate::touch`] and reports it with [`SaveGate::fire`], so it is deterministic to test.
///
/// * every edit [`touch`](SaveGate::touch)es the gate and gets a generation number;
/// * when the timer of the *latest* generation fires, a write starts ([`fire`](SaveGate::fire));
/// * edits made while a write is in flight never start a second one: the follow-up write starts
///   from [`finish`](SaveGate::finish).
#[derive(Debug, Default)]
pub struct SaveGate {
    generation: u64,
    dirty: bool,
    in_flight: bool,
    resave: bool,
}

impl SaveGate {
    /// An edit happened. Returns the generation to arm the debounce timer with.
    pub fn touch(&mut self) -> u64 {
        self.dirty = true;
        self.generation += 1;
        self.generation
    }

    /// The debounce timer of `generation` fired. `true` = start writing now.
    pub fn fire(&mut self, generation: u64) -> bool {
        if generation != self.generation || !self.dirty {
            return false; // superseded by a later edit (its own timer is pending)
        }
        if self.in_flight {
            self.resave = true;
            return false;
        }
        self.begin();
        true
    }

    /// Write right now (closing the window): `true` when there is something to write and no
    /// write is in flight.
    pub fn force(&mut self) -> bool {
        if !self.dirty || self.in_flight {
            return false;
        }
        self.begin();
        true
    }

    /// The write that was in flight ended. `true` = edits arrived meanwhile, write again now.
    pub fn finish(&mut self) -> bool {
        self.in_flight = false;
        if self.resave && self.dirty {
            self.resave = false;
            self.begin();
            return true;
        }
        self.resave = false;
        false
    }

    fn begin(&mut self) {
        self.dirty = false;
        self.in_flight = true;
    }

    /// Unsaved edits or a write in progress.
    pub fn is_pending(&self) -> bool {
        self.dirty || self.in_flight
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight
    }
}

fn to_value(c: &Config) -> Option<Value> {
    Value::try_from(c).ok()
}

/// Three-way merge of a hot-reloaded config: `base` is what was last on disk (as far as we know),
/// `ours` the working copy (possibly with unsaved edits) and `theirs` the new file content.
///
/// Per key: untouched by us -> take theirs; touched only by us -> keep ours; touched by both ->
/// ours (our edit is about to be saved and wins). Tables merge recursively, so an edit to
/// `appearance.accent` does not hold back an external change to `appearance.mode`.
/// If the result is invalid the file's version wins.
pub fn merge_external(base: &Config, ours: &Config, theirs: &Config) -> Config {
    if ours == base {
        return theirs.clone();
    }
    if theirs == base {
        return ours.clone();
    }
    let (Some(b), Some(o), Some(t)) = (to_value(base), to_value(ours), to_value(theirs)) else {
        return theirs.clone();
    };
    let merged = merge_value(Some(&b), Some(&o), Some(&t));
    let parsed = merged.and_then(|v| v.try_into::<Config>().ok());
    match parsed {
        Some(c) if c.validate().is_empty() => c,
        _ => theirs.clone(),
    }
}

fn merge_value(b: Option<&Value>, o: Option<&Value>, t: Option<&Value>) -> Option<Value> {
    if o == b {
        return t.cloned();
    }
    if t == b {
        return o.cloned();
    }
    match (o, t) {
        (Some(Value::Table(ot)), Some(Value::Table(tt))) => {
            let bt = match b {
                Some(Value::Table(bt)) => Some(bt),
                _ => None,
            };
            let mut keys: Vec<&String> = ot.keys().chain(tt.keys()).collect();
            keys.sort();
            keys.dedup();
            let mut out = toml::map::Map::new();
            for k in keys {
                if let Some(v) = merge_value(bt.and_then(|m| m.get(k)), ot.get(k), tt.get(k)) {
                    out.insert(k.clone(), v);
                }
            }
            Some(Value::Table(out))
        }
        _ => o.cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::config::ThemeMode;

    #[test]
    fn debounce_collapses_a_burst_into_one_write() {
        let mut g = SaveGate::default();
        let g1 = g.touch();
        let g2 = g.touch();
        let g3 = g.touch();
        assert!(!g.fire(g1), "superseded timers do nothing");
        assert!(!g.fire(g2));
        assert!(g.fire(g3), "the last timer starts the write");
        assert!(g.in_flight());
        assert!(!g.fire(g3), "and only once");
        assert!(!g.finish());
        assert!(!g.is_pending());
    }

    #[test]
    fn single_write_in_flight_then_one_follow_up() {
        let mut g = SaveGate::default();
        let a = g.touch();
        assert!(g.fire(a));
        // Edits while writing: timers fire but no second write starts.
        let b = g.touch();
        assert!(!g.fire(b));
        let c = g.touch();
        assert!(!g.fire(c));
        assert!(g.in_flight());
        // When the first write ends, exactly one follow-up begins.
        assert!(g.finish());
        assert!(g.in_flight());
        assert!(!g.finish());
        assert!(!g.is_pending());
    }

    #[test]
    fn edit_during_write_whose_timer_fires_after_finish() {
        let mut g = SaveGate::default();
        let a = g.touch();
        assert!(g.fire(a));
        let b = g.touch();
        assert!(!g.finish(), "the follow-up waits for its own timer");
        assert!(g.is_dirty());
        assert!(g.fire(b));
    }

    #[test]
    fn force_flushes_only_when_needed() {
        let mut g = SaveGate::default();
        assert!(!g.force());
        g.touch();
        assert!(g.force());
        assert!(!g.force(), "already in flight");
    }

    fn cfg(f: impl FnOnce(&mut Config)) -> Config {
        let mut c = Config::default();
        f(&mut c);
        c
    }

    #[test]
    fn external_change_to_another_key_is_taken_and_our_edit_kept() {
        let base = Config::default();
        let ours = cfg(|c| c.appearance.accent = "#112233".into());
        let theirs = cfg(|c| c.appearance.mode = ThemeMode::Light);
        let m = merge_external(&base, &ours, &theirs);
        assert_eq!(m.appearance.accent, "#112233");
        assert_eq!(m.appearance.mode, ThemeMode::Light);
    }

    #[test]
    fn same_key_edited_on_both_sides_keeps_ours() {
        let base = Config::default();
        let ours = cfg(|c| c.terminal.scrollback = 5_000);
        let theirs = cfg(|c| c.terminal.scrollback = 7_000);
        assert_eq!(merge_external(&base, &ours, &theirs).terminal.scrollback, 5_000);
    }

    #[test]
    fn no_local_edits_adopts_the_file() {
        let base = Config::default();
        let theirs = cfg(|c| {
            c.git.confirm_sensitive = false;
            c.keybindings.insert("tab.new".into(), "ctrl+t".into());
        });
        assert_eq!(merge_external(&base, &base, &theirs), theirs);
    }

    #[test]
    fn echo_of_our_own_save_changes_nothing() {
        let base = Config::default();
        let ours = cfg(|c| c.terminal.scrollback = 5_000);
        // The watcher reports what we saved, but we already moved on.
        let saved = ours;
        let ours_now = cfg(|c| {
            c.terminal.scrollback = 5_000;
            c.git.confirm_sensitive = false;
        });
        let m = merge_external(&base, &ours_now, &saved);
        assert_eq!(m, ours_now);
    }

    #[test]
    fn list_edits_survive() {
        let base = Config::default();
        let ours = cfg(|c| {
            c.profiles.push(store::config::Profile {
                name: "Dev".into(),
                program: "pwsh.exe".into(),
                ..Default::default()
            })
        });
        let theirs = cfg(|c| c.appearance.mono_size = 15.0);
        let m = merge_external(&base, &ours, &theirs);
        assert_eq!(m.profiles.len(), 1);
        assert_eq!(m.appearance.mono_size, 15.0);
    }
}
