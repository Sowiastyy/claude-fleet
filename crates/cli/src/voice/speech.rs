//! Text on its way to and from the ear.
//!
//! Outbound, a reply written for a screen becomes something worth hearing:
//! markdown marks, code, tables and long paths are what a speech synthesizer
//! reads worst, so they are dropped or shortened, and what is left is cut into
//! sentences that can be spoken one after another while the next is rendered.
//!
//! Inbound, what the recogniser heard is sorted: noise Whisper is known to
//! invent out of silence, the fleet's own voice caught by the microphone, the
//! few words that steer the fleet itself, and everything else, which is a
//! message for Claude.

/// What a piece of spoken input asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Stop talking; the session keeps working.
    Silence,
    /// Stop the session's turn, the way Esc does.
    Interrupt,
    /// Say the last reply again.
    Repeat,
    /// Answer the question a session is stopped on: yes, with anything said
    /// after it sent once the turn goes on…
    Allow(Option<String>),
    /// …or no, with what to do instead.
    Deny(Option<String>),
    /// Put the named session on the pane.
    Switch(String),
    /// Start a session where the selected one works.
    NewSession,
    /// Stop listening.
    VoiceOff,
    /// Start, or stop, the companion: a fast session that is the one talked
    /// to and hands the work to the others.
    Companion(bool),
    /// Anything else goes to Claude as it was said.
    Message(String),
}

/// Words that open an answer to a permission prompt.
/// Not "zrób to": "zrób to w osobnym pliku" is a change, not a yes.
const YES: &[&str] = &[
    "tak", "zgoda", "zgadzam sie", "pozwol", "pozwalam", "dawaj", "jasne", "ok", "okej", "mozesz",
    "yes", "allow",
];
/// Not "no": in Polish it opens agreement as often as anything ("no tak").
const NO: &[&str] = &["nie", "odmow", "nie pozwalam", "deny"];

/// The longest utterance still read as a command. Anything longer is a
/// sentence meant for Claude, even when it starts with a command word:
/// "stop using unwrap in the parser" is not "stop".
const COMMAND_WORDS: usize = 4;

/// Sort one recognised utterance. `waiting` says whether the session it would
/// go to is stopped on a question, the only time yes and no are answers.
pub fn intent(heard: &str, waiting: bool) -> Intent {
    let norm = normalize(heard);
    let words: Vec<&str> = norm.split_whitespace().collect();
    if words.is_empty() {
        return Intent::Message(String::new());
    }

    if waiting {
        // "nie, zrób to w osobnym pliku" is a no carrying the instruction the
        // denial asks for; the rest of the sentence is kept as it was said.
        if let Some(rest) = strip_lead(heard, &norm, NO) {
            return Intent::Deny((!rest.is_empty()).then_some(rest));
        }
        // "tak, a potem puść testy": the rest follows the yes. A lone word
        // after it ("tak, dzięki") is manners, not a message.
        if let Some(rest) = strip_lead(heard, &norm, YES) {
            return Intent::Allow((rest.split_whitespace().count() >= 2).then_some(rest));
        }
    }

    if words.len() <= COMMAND_WORDS {
        let is = |set: &[&str]| set.iter().any(|p| norm == *p);
        if is(&[
            "stop", "cisza", "cicho", "przestan", "przestan mowic", "wystarczy", "dosc", "dobra",
            "dobra dzieki", "dzieki", "ok dzieki", "shut up", "quiet",
        ]) {
            return Intent::Silence;
        }
        if is(&["przerwij", "anuluj", "zatrzymaj", "zatrzymaj sie", "cancel", "interrupt"]) {
            return Intent::Interrupt;
        }
        if is(&["powtorz", "powtorz prosze", "jeszcze raz", "co mowiles", "repeat"]) {
            return Intent::Repeat;
        }
        if is(&["nowa sesja", "otworz nowa sesje", "new session"]) {
            return Intent::NewSession;
        }
        if is(&[
            "wylacz glos",
            "wylacz sluchanie",
            "koniec rozmowy",
            "przestan sluchac",
            "voice off",
        ]) {
            return Intent::VoiceOff;
        }
        if let Some(on) = companion_switch(&words) {
            return Intent::Companion(on);
        }
        for lead in [
            "przelacz na sesje",
            "przelacz na",
            "przejdz do sesji",
            "przejdz do",
            "sesja",
            "switch to",
        ] {
            // Whole words only: "sesjami" is not "sesja mi".
            if let Some(name) = norm.strip_prefix(&format!("{lead} ")) {
                let name = name.trim();
                if !name.is_empty() {
                    return Intent::Switch(name.to_string());
                }
            }
        }
    }
    if waiting {
        // Anything else said to a session stopped on a question is an answer
        // other than the ones offered. Typed into the dialog, its letters and
        // digits would pick options; declining and sending the words is what
        // the dialog's own "tell Claude what to do instead" does.
        return Intent::Deny(Some(heard.trim().to_string()));
    }
    Intent::Message(heard.trim().to_string())
}

/// Whether a short utterance turns the companion on or off. It is taken by
/// its shape rather than its letters, since the recogniser hears "włącz
/// rozmówcę" as "włąd rozmówce" as readily: a word that begins the way
/// "włącz" or "wyłącz" does, then the companion by any of its names.
fn companion_switch(words: &[&str]) -> Option<bool> {
    let (name, before) = words.split_last()?;
    if !name.starts_with("rozmowc") && !matches!(*name, "pomocnika" | "asystenta" | "companion") {
        return None;
    }
    match before {
        ["turn", "on"] | ["turn", "on", "the"] | ["start", "the"] => Some(true),
        ["turn", "off"] | ["turn", "off", "the"] | ["stop", "the"] => Some(false),
        [verb] if verb.starts_with("wyl") || matches!(*verb, "zamknij" | "stop") => Some(false),
        [verb] if verb.starts_with("wl") || matches!(*verb, "uruchom" | "daj" | "start") => {
            Some(true)
        }
        _ => None,
    }
}

/// When `norm` opens with one of `set`, what follows it in the original
/// wording, punctuation around the cut trimmed.
fn strip_lead(original: &str, norm: &str, set: &[&str]) -> Option<String> {
    let lead = set
        .iter()
        .filter(|p| norm == **p || norm.starts_with(&format!("{p} ")))
        .max_by_key(|p| p.len())?;
    let skip = lead.split_whitespace().count();
    let rest: Vec<&str> = original.split_whitespace().skip(skip).collect();
    let rest = rest.join(" ");
    Some(
        rest.trim_matches(|c: char| c.is_whitespace() || ",.;:!?-–—".contains(c))
            .to_string(),
    )
}

/// Lowercase, Polish letters folded to ASCII, punctuation gone, spaces
/// single: the form commands, echoes and hallucinations are compared in.
pub fn normalize(s: &str) -> String {
    let folded: String = s
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Phrases Whisper produces out of silence, breath or a keyboard. They come
/// from the subtitles it was trained on, so they are the closing lines of
/// videos rather than anything a person says to a terminal.
const HALLUCINATIONS: &[&str] = &[
    "dziekuje za uwage",
    "dziekuje za obejrzenie",
    "dzieki za obejrzenie",
    "dziekuje bardzo",
    "dziekuje",
    "napisy stworzone przez spolecznosc amara org",
    "napisy wykonane przez",
    "subskrybuj",
    "subskrybujcie",
    "zasubskrybuj kanal",
    "do zobaczenia",
    "do zobaczenia w nastepnym odcinku",
    "thank you",
    "thanks for watching",
    "thank you for watching",
    "you",
    "bye",
    "muzyka",
    "napisy",
    "transkrypcja",
    // A cough, written down as one.
    "cough",
    "coughs",
    "coughing",
    "kaszel",
    "kaszle",
    "ekhem",
];

/// Whether a transcript is more likely Whisper's invention than speech.
pub fn is_hallucination(text: &str) -> bool {
    let norm = normalize(text);
    if norm.is_empty() {
        return true;
    }
    if HALLUCINATIONS.contains(&norm.as_str()) {
        return true;
    }
    if norm.contains("amara org") || norm.starts_with("napisy ") {
        return true;
    }
    // Bracketed sound tags: [muzyka], (śmiech), *kaszel*.
    let t = text.trim();
    let tagged = |open: char, close: char| t.starts_with(open) && t.ends_with(close);
    tagged('[', ']') || tagged('(', ')') || tagged('*', '*') || tagged('♪', '♪')
}

/// How much of `heard` was also in something the fleet said: the share of
/// its words found there. The microphone picking up the speaker scores near
/// one; a person answering scores near zero.
pub fn echo_score(heard: &str, said: &str) -> f32 {
    let heard = normalize(heard);
    let said = normalize(said);
    let words: Vec<&str> = heard.split_whitespace().collect();
    if words.is_empty() {
        return 0.0;
    }
    let pool: std::collections::HashSet<&str> = said.split_whitespace().collect();
    let hits = words.iter().filter(|w| pool.contains(*w)).count();
    hits as f32 / words.len() as f32
}

/// A reply as it is worth hearing, or `None` when nothing in it is.
///
/// Fenced code and tables are dropped with one short note that there is
/// something on screen; inline code keeps its words; paths shrink to their
/// file name and links to a word. Past `max_chars` the rest is left to the
/// screen, cut at a sentence.
pub fn speakable(markdown: &str, max_chars: usize, lang: &str) -> Option<String> {
    let phrases = Phrases::for_lang(lang);
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;
    let mut noted_code = false;
    let mut noted_table = false;

    for raw in markdown.lines() {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            if !in_fence && !noted_code {
                out.push(phrases.code_on_screen.to_string());
                noted_code = true;
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if line.starts_with('|') {
            if !noted_table {
                out.push(phrases.table_on_screen.to_string());
                noted_table = true;
            }
            continue;
        }
        if line.is_empty() || line.chars().all(|c| "-*_=#>".contains(c) || c.is_whitespace()) {
            continue;
        }
        let line = strip_line_marks(line);
        let line = clean_inline(&line);
        if line.is_empty() {
            continue;
        }
        // A heading or a list item carries no full stop of its own; without
        // one the synthesizer runs it into the next line.
        let ends = line.ends_with(['.', '!', '?', ':', ';', '…']);
        out.push(if ends { line } else { format!("{line}.") });
    }

    let text = out.join(" ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalize(&text).is_empty() {
        return None;
    }
    if text.chars().count() <= max_chars {
        return Some(text);
    }
    let mut kept = String::new();
    for s in sentences(&text) {
        if !kept.is_empty() && kept.chars().count() + s.chars().count() > max_chars {
            break;
        }
        if !kept.is_empty() {
            kept.push(' ');
        }
        kept.push_str(&s);
    }
    kept.push(' ');
    kept.push_str(phrases.rest_on_screen);
    Some(kept)
}

/// Heading hashes, list bullets and numbers, quote marks at a line's start.
fn strip_line_marks(line: &str) -> String {
    let mut l = line.trim_start_matches('#').trim_start();
    l = l.trim_start_matches('>').trim_start();
    for bullet in ["- [ ] ", "- [x] ", "- ", "* ", "+ ", "• "] {
        if let Some(rest) = l.strip_prefix(bullet) {
            l = rest;
            break;
        }
    }
    // "1. " and "12) " numbering.
    let digits = l.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && digits < 4 {
        let after = &l[digits..];
        if let Some(rest) = after.strip_prefix(". ").or_else(|| after.strip_prefix(") ")) {
            l = rest;
        }
    }
    l.to_string()
}

/// Emphasis, backticks, links, paths and emoji out of one line.
fn clean_inline(line: &str) -> String {
    let s = line.replace("**", "").replace("__", "").replace('`', "");
    let s = strip_links(&s);
    let words: Vec<String> = s
        .split_whitespace()
        .map(speak_word)
        .filter(|w| !w.is_empty())
        .collect();
    let s = words.join(" ");
    s.chars()
        .filter(|c| {
            !matches!(*c as u32,
                0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D)
        })
        .filter(|c| *c != '*' && *c != '`')
        .collect::<String>()
        .trim()
        .to_string()
}

/// `[text](url)` keeps the text; a bare URL becomes the word "link".
fn strip_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](").map(|i| open + i) else {
            break;
        };
        let Some(end) = rest[close..].find(')').map(|i| close + i) else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..close]);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// One word as a voice should say it: a path is its file name, a URL is
/// "link", an identifier's underscores are spaces.
fn speak_word(w: &str) -> String {
    let (core, tail) = split_trailing_punct(w);
    if core.starts_with("http://") || core.starts_with("https://") {
        return format!("link{tail}");
    }
    let core = if looks_like_path(core) {
        core.rsplit(['/', '\\'])
            .find(|p| !p.is_empty())
            .unwrap_or(core)
            .to_string()
    } else {
        // "busy/idle" is two words with a slash between, not a file.
        core.replace(['/', '\\'], " ")
    };
    let core = core.replace('_', " ");
    format!("{core}{tail}")
}

/// A path is a word with a separator that either runs through several
/// directories, starts where paths start, or ends in a file extension.
fn looks_like_path(w: &str) -> bool {
    let seps = w.matches(['/', '\\']).count();
    if seps == 0 {
        return false;
    }
    let last = w.rsplit(['/', '\\']).next().unwrap_or_default();
    let has_ext = last.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && (1..=5).contains(&ext.len()) && ext.chars().all(char::is_alphanumeric)
    });
    let rooted = w.starts_with("~/")
        || w.starts_with("./")
        || w.starts_with("../")
        || w.get(1..3) == Some(":\\")
        || w.get(1..3) == Some(":/");
    seps >= 2 || has_ext || rooted
}

fn split_trailing_punct(w: &str) -> (&str, &str) {
    let cut = w
        .char_indices()
        .rev()
        .take_while(|(_, c)| ",.;:!?)".contains(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(w.len());
    // A file name keeps its extension: "app.rs." ends a sentence, "app.rs" does not.
    (&w[..cut], &w[cut..])
}

/// Cut text into sentences, each a unit the synthesizer can start on while
/// the one before it plays. Very long ones are split at a comma.
pub fn sentences(text: &str) -> Vec<String> {
    const LONG: usize = 220;
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        let next = chars.get(i + 1).copied();
        // "app.rs" and "1.5" are not sentence ends: only a mark followed by a
        // space, or by nothing, is.
        let boundary = matches!(c, '.' | '!' | '?' | '…') && next.is_none_or(char::is_whitespace);
        let long_comma = c == ',' && cur.chars().count() > LONG;
        if boundary || long_comma {
            let t = cur.trim().to_string();
            if !t.is_empty() {
                out.push(t);
            }
            cur.clear();
        }
    }
    let t = cur.trim().to_string();
    if !t.is_empty() {
        out.push(t);
    }
    out
}

/// What the fleet says in its own words, per language.
pub struct Phrases {
    pub code_on_screen: &'static str,
    pub table_on_screen: &'static str,
    pub rest_on_screen: &'static str,
    pub listening: &'static str,
    pub no_session: &'static str,
    pub session: &'static str,
    pub finished: &'static str,
    pub asks: &'static str,
    pub permission: &'static str,
    pub permission_for: &'static str,
    pub say_yes_no: &'static str,
    pub claude_asks: &'static str,
    pub claude_waits: &'static str,
    pub nothing_to_repeat: &'static str,
    pub no_such_session: &'static str,
    pub switched: &'static str,
    pub interrupted: &'static str,
    pub trust: &'static str,
    pub companion_on: &'static str,
    pub companion_off: &'static str,
    pub companion_failed: &'static str,
}

impl Phrases {
    pub fn for_lang(lang: &str) -> &'static Phrases {
        if lang.starts_with("pl") { &PL } else { &EN }
    }
}

static PL: Phrases = Phrases {
    code_on_screen: "Kod jest na ekranie.",
    table_on_screen: "Tabela jest na ekranie.",
    rest_on_screen: "Resztę masz na ekranie.",
    listening: "Słucham.",
    no_session: "Nie ma aktywnej sesji. Powiedz: nowa sesja.",
    session: "Sesja",
    finished: "skończyła.",
    asks: "czeka na odpowiedź.",
    permission: "Claude pyta o zgodę.",
    permission_for: "Claude pyta o zgodę na",
    say_yes_no: "Powiedz tak albo nie.",
    claude_asks: "Claude pyta:",
    claude_waits: "Claude czeka na odpowiedź.",
    nothing_to_repeat: "Nie mam czego powtórzyć.",
    no_such_session: "Nie znam takiej sesji.",
    switched: "Jesteś w sesji",
    interrupted: "Przerwane.",
    trust: "Claude pyta, czy ufasz temu folderowi.",
    companion_on: "Rozmówca włączony. Mów do mnie, a pracę przekażę sesjom.",
    companion_off: "Rozmówca wyłączony. Mówisz teraz prosto do sesji.",
    companion_failed: "Nie udało się uruchomić rozmówcy.",
};

static EN: Phrases = Phrases {
    code_on_screen: "The code is on screen.",
    table_on_screen: "The table is on screen.",
    rest_on_screen: "The rest is on screen.",
    listening: "Listening.",
    no_session: "There is no session. Say: new session.",
    session: "Session",
    finished: "has finished.",
    asks: "is waiting for an answer.",
    permission: "Claude is asking for permission.",
    permission_for: "Claude is asking to run",
    say_yes_no: "Say yes or no.",
    claude_asks: "Claude asks:",
    claude_waits: "Claude is waiting for an answer.",
    nothing_to_repeat: "Nothing to repeat.",
    no_such_session: "No session by that name.",
    switched: "Now in session",
    interrupted: "Interrupted.",
    trust: "Claude asks whether you trust this folder.",
    companion_on: "The companion is on. Talk to me, and I will hand the work to the sessions.",
    companion_off: "The companion is off. You are talking straight to the session now.",
    companion_failed: "The companion did not start.",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_loses_its_markdown_and_keeps_its_words() {
        let md = "## Wynik\n\nZnalazłem **dwa** błędy w `src/app.rs`:\n\n- brak obsługi `spawn_session`\n- zły [link](https://x.y/z)\n";
        let s = speakable(md, 1000, "pl").unwrap();
        assert_eq!(
            s,
            "Wynik. Znalazłem dwa błędy w app.rs: brak obsługi spawn session. zły link."
        );
    }

    #[test]
    fn code_and_tables_are_noted_once_not_read() {
        let md = "Poprawka:\n```rust\nfn main() {}\n```\nI druga:\n```\nx\n```\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let s = speakable(md, 1000, "pl").unwrap();
        assert_eq!(
            s,
            "Poprawka: Kod jest na ekranie. I druga: Tabela jest na ekranie."
        );
    }

    #[test]
    fn a_long_reply_is_cut_at_a_sentence_and_points_at_the_screen() {
        let md = "Pierwsze zdanie jest tutaj. Drugie zdanie też. Trzecie zdanie już się nie zmieści.";
        let s = speakable(md, 50, "pl").unwrap();
        assert_eq!(
            s,
            "Pierwsze zdanie jest tutaj. Drugie zdanie też. Resztę masz na ekranie."
        );
    }

    #[test]
    fn nothing_but_code_says_so_rather_than_nothing() {
        assert_eq!(
            speakable("```\nls\n```", 100, "pl").as_deref(),
            Some("Kod jest na ekranie.")
        );
        assert_eq!(speakable("  \n---\n", 100, "pl"), None);
    }

    #[test]
    fn sentences_split_at_ends_not_inside_file_names() {
        let s = sentences("Sprawdź app.rs teraz. Wersja 1.5 działa! Gotowe?");
        assert_eq!(s, vec!["Sprawdź app.rs teraz.", "Wersja 1.5 działa!", "Gotowe?"]);
    }

    #[test]
    fn short_words_steer_the_fleet_long_ones_go_to_claude() {
        assert_eq!(intent("Stop.", false), Intent::Silence);
        assert_eq!(intent("Cisza!", false), Intent::Silence);
        assert_eq!(intent("Przerwij", false), Intent::Interrupt);
        assert_eq!(intent("Powtórz.", false), Intent::Repeat);
        assert_eq!(intent("Nowa sesja", false), Intent::NewSession);
        assert_eq!(
            intent("Przełącz na wanderers", false),
            Intent::Switch("wanderers".into())
        );
        assert_eq!(
            intent("Stop używania unwrap w parserze, proszę.", false),
            Intent::Message("Stop używania unwrap w parserze, proszę.".into())
        );
    }

    #[test]
    fn yes_and_no_answer_only_a_waiting_session() {
        assert_eq!(intent("Tak.", true), Intent::Allow(None));
        assert_eq!(intent("Tak, dawaj", true), Intent::Allow(None));
        assert_eq!(
            intent("Tak, a potem uruchom testy.", true),
            Intent::Allow(Some("a potem uruchom testy".into()))
        );
        assert_eq!(intent("Tak.", false), Intent::Message("Tak.".into()));
        assert_eq!(intent("Nie.", true), Intent::Deny(None));
        assert_eq!(
            intent("Nie, zrób to w osobnym pliku.", true),
            Intent::Deny(Some("zrób to w osobnym pliku".into()))
        );
    }

    #[test]
    fn other_words_to_a_waiting_session_decline_and_say_them() {
        assert_eq!(
            intent("Zrób to w osobnym pliku", true),
            Intent::Deny(Some("Zrób to w osobnym pliku".into()))
        );
        // The fleet's own commands still work over a dialog.
        assert_eq!(intent("Stop.", true), Intent::Silence);
        assert_eq!(intent("Powtórz", true), Intent::Repeat);
    }

    #[test]
    fn the_companion_is_turned_on_and_off_by_name() {
        assert_eq!(intent("Włącz rozmówcę.", false), Intent::Companion(true));
        assert_eq!(intent("wyłącz rozmówcę", false), Intent::Companion(false));
        assert_eq!(intent("Turn on the companion", false), Intent::Companion(true));
        // As the recogniser has written it down.
        assert_eq!(intent("włąd, rozmówce.", false), Intent::Companion(true));
        assert_eq!(intent("Wyłącz rozmówca", false), Intent::Companion(false));
        assert!(matches!(intent("rozmówca", false), Intent::Message(_)));
        // A sentence about it is a message, not the switch.
        assert!(matches!(
            intent("włącz rozmówcę kiedy skończysz te testy", false),
            Intent::Message(_)
        ));
    }

    #[test]
    fn whisper_inventions_are_recognised() {
        assert!(is_hallucination("Dziękuję za uwagę."));
        assert!(is_hallucination(" Napisy stworzone przez społeczność Amara.org"));
        assert!(is_hallucination("[muzyka]"));
        assert!(is_hallucination("..."));
        assert!(is_hallucination("Cough,"));
        assert!(!is_hallucination("Dziękuję, a teraz sprawdź testy."));
    }

    #[test]
    fn the_fleets_own_voice_scores_as_echo() {
        let said = "Znalazłem dwa błędy w pliku app.rs.";
        assert!(echo_score("znalazłem dwa błędy w pliku", said) > 0.9);
        assert!(echo_score("a co z testami?", said) < 0.3);
    }
}
