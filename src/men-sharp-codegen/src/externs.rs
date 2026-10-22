//! The Udon extern whitelist, from an SDK dump.
//!
//! A dump (`data/udon/<unity version>.json`, see its README for provenance)
//! enumerates every node definition the VRChat SDK exposes. One is embedded
//! into the compiler binary — no downloads, no setup steps — and parsed
//! lazily; the Unity package can hand over a fresher one, dumped from the SDK
//! the project actually has installed (a beta, a closed-beta SDK with
//! externs of its own), with [`UdonNodes::load`]. Either answers the two
//! questions code generation asks constantly:
//!
//! - *does this extern signature exist?* (and how many parameters it pops)
//! - *is this type usable on the Udon heap?*
//!
//! Signature strings look like
//! `SystemString.__Concat__SystemString_SystemString__SystemString`; heap type
//! names like `SystemInt32` are .NET full names with `.`, `+` and generic
//! backticks squeezed out — [`mangle_dotnet_name`] implements that rule.

use rustc_hash::FxHashMap as HashMap;
use std::fmt;
use std::path::Path;
use std::sync::OnceLock;

/// One embedded dump per supported Unity editor version, newest last.
const DUMPS: &[(&str, &str)] = &[(
    "2022.3.22f1",
    include_str!("../../../data/udon/2022.3.22f1.json"),
)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterKind {
    In,
    Out,
    InOut,
}

#[derive(Debug, Clone)]
pub struct ExternNode {
    pub parameters: Vec<ParameterKind>,
}

/// One value a built-in event hands to the program: before raising the event,
/// the runtime writes it into the heap slot named after event and parameter
/// (`onPlayerJoined` + `Player` → `onPlayerJoinedPlayer`).
#[derive(Debug, Clone)]
pub struct EventParameter {
    pub name: String,
    /// The .NET full name, as the dump spells it (`VRC.SDKBase.VRCPlayerApi`).
    pub dotnet_type: String,
}

/// A built-in event (`Event_OnPlayerJoined`, stored without the prefix).
#[derive(Debug, Clone)]
pub struct EventNode {
    pub parameters: Vec<EventParameter>,
}

/// The whitelist for one Unity version.
pub struct UdonNodes {
    pub unity_version: String,
    /// The SDK the dump was taken from, as the dumper names it
    /// (`com.vrchat.worlds 3.10.5`). Only a project's own dump says; the
    /// embedded one does not, and diagnostics tell the two apart by it.
    pub sdk: Option<String>,
    externs: HashMap<String, ExternNode>,
    /// Built-in events by their `Event_`-less name (`OnPlayerJoined`).
    events: HashMap<String, EventNode>,
    /// Mangled names of types the heap can declare (`Type_…` nodes).
    types: rustc_hash::FxHashSet<String>,
}

impl UdonNodes {
    /// The embedded whitelist for the requested Unity version; an exact
    /// match if we have one, otherwise the newest embedded dump.
    pub fn for_unity_version(version: Option<&str>) -> &'static UdonNodes {
        static PARSED: OnceLock<Vec<UdonNodes>> = OnceLock::new();
        let all = PARSED.get_or_init(|| {
            DUMPS
                .iter()
                .map(|(version, json)| {
                    let mut nodes = UdonNodes::parse(json)
                        .unwrap_or_else(|error| panic!("embedded dump {version}: {error}"));
                    nodes.unity_version = version.to_string();
                    // exported from a project like any other dump, but not
                    // *this* project's SDK: diagnostics must not say it is
                    nodes.sdk = None;
                    nodes
                })
                .collect()
        });
        version
            .and_then(|v| all.iter().find(|nodes| nodes.unity_version == v))
            .unwrap_or_else(|| all.last().expect("at least one dump is embedded"))
    }

    /// A dump the SDK in a project produced (`MenSharp/Dump Udon Node
    /// Definitions`, or the Unity package doing the same on its own).
    pub fn load(path: &Path) -> Result<UdonNodes, DumpError> {
        let json = std::fs::read_to_string(path).map_err(|error| DumpError {
            offset: None,
            message: error.to_string(),
        })?;
        Self::parse(&json)
    }

    /// Reads a dump. Its schema is ours, so anything unexpected in it is an
    /// error rather than something to skip: a file that is not a dump, or
    /// was cut short, must not pass for a whitelist with half the externs.
    pub fn parse(json: &str) -> Result<UdonNodes, DumpError> {
        parse_dump(json)
    }

    pub fn extern_node(&self, signature: &str) -> Option<&ExternNode> {
        self.externs.get(signature)
    }

    /// The built-in event a method of this name would handle, if any.
    pub fn event(&self, name: &str) -> Option<&EventNode> {
        self.events.get(name)
    }

    pub fn has_signature(&self, signature: &str) -> bool {
        self.externs.contains_key(signature)
    }

    pub fn has_type(&self, mangled: &str) -> bool {
        self.types.contains(mangled)
    }

    pub fn extern_count(&self) -> usize {
        self.externs.len()
    }
}

/// Why a dump could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpError {
    /// The byte the reader stopped at; `None` when the file itself could
    /// not be read.
    pub offset: Option<usize>,
    pub message: String,
}

impl fmt::Display for DumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.offset {
            Some(offset) => write!(f, "{} (at byte {offset})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for DumpError {}

/// A *flat* .NET name (`Namespace.Outer+Nested`, `List` + backtick arity) →
/// its Udon spelling: separators vanish, a generic arity suffix vanishes.
/// Generic instantiations are mangled structurally by the code generator —
/// mangle the definition and each argument, then concatenate
/// (`SystemCollectionsGenericList` + `SystemInt32`).
pub fn mangle_dotnet_name(full: &str) -> String {
    let mut out = String::with_capacity(full.len());
    let mut in_arity = false;
    for c in full.chars() {
        match c {
            '`' => in_arity = true,
            c if in_arity && c.is_ascii_digit() => {}
            c if c.is_ascii_alphanumeric() || c == '_' => {
                in_arity = false;
                out.push(c);
            }
            _ => in_arity = false,
        }
    }
    out
}

// ---------------------------------------------------------------- the parser
//
// The dump's schema is fixed (we generate it), so this is a minimal JSON
// reader specialised to it: it walks the `nodes` array and keeps `fullName`
// plus each parameter's `kind`. Anything unexpected is an error — the input
// may be a project's own dump, read at run time.

type Parsed<T> = Result<T, DumpError>;

fn parse_dump(json: &str) -> Parsed<UdonNodes> {
    let mut reader = Reader {
        text: json,
        bytes: json.as_bytes(),
        at: 0,
    };
    // a UTF-8 byte-order mark, which an editor on Windows may have added
    if json.starts_with('\u{feff}') {
        reader.at = '\u{feff}'.len_utf8();
    }
    // about one node per 300 bytes of dump; sized up front so the maps never
    // rehash while they fill (2022.3.22f1: ~36k externs, ~2k types)
    let nodes_estimate = json.len() / 300;
    let mut nodes = UdonNodes {
        unity_version: String::new(),
        sdk: None,
        externs: HashMap::with_capacity_and_hasher(nodes_estimate, Default::default()),
        events: HashMap::default(),
        types: rustc_hash::FxHashSet::with_capacity_and_hasher(
            nodes_estimate / 16,
            Default::default(),
        ),
    };
    let mut saw_nodes = false;

    reader.expect(b'{')?;
    loop {
        let key = reader.key()?;
        reader.expect(b':')?;
        match key {
            "unityVersion" => {
                nodes.unity_version = reader.string()?;
            }
            "sdk" => {
                nodes.sdk = Some(reader.string()?);
            }
            "nodes" => {
                saw_nodes = true;
                reader.expect(b'[')?;
                if !reader.consume(b']') {
                    loop {
                        parse_node(&mut reader, &mut nodes)?;
                        if !reader.consume(b',') {
                            break;
                        }
                    }
                    reader.expect(b']')?;
                }
            }
            other => return Err(reader.error(format!("unexpected key {other:?}"))),
        }
        if !reader.consume(b',') {
            break;
        }
    }
    reader.expect(b'}')?;
    reader.skip_whitespace();
    if reader.at != reader.bytes.len() {
        return Err(reader.error("text after the end of the dump".to_string()));
    }
    // an empty whitelist would turn every extern into an error, all of
    // them pointing at the user's code rather than at the dump
    if !saw_nodes || nodes.externs.is_empty() {
        return Err(DumpError {
            offset: None,
            message: "the dump lists no externs".to_string(),
        });
    }
    Ok(nodes)
}

fn parse_node(reader: &mut Reader, nodes: &mut UdonNodes) -> Parsed<()> {
    let mut full_name = String::new();
    let mut parameters = Vec::new();

    reader.expect(b'{')?;
    loop {
        let key = reader.key()?;
        reader.expect(b':')?;
        match key {
            "fullName" => full_name = reader.string()?,
            "relatedType" => {
                if !reader.consume_null() {
                    reader.skip_string()?;
                }
            }
            "parameters" => {
                // parameter names and types are only kept for events; an
                // extern's are dropped, so they are not even copied out.
                // (`fullName` comes first in the dump; if it ever did not,
                // the names would be read and dropped, as before.)
                let keep_names = full_name.is_empty() || full_name.starts_with("Event_");
                reader.expect(b'[')?;
                if !reader.consume(b']') {
                    loop {
                        parameters.push(parse_parameter(reader, keep_names)?);
                        if !reader.consume(b',') {
                            break;
                        }
                    }
                    reader.expect(b']')?;
                }
            }
            other => return Err(reader.error(format!("unexpected node key {other:?}"))),
        }
        if !reader.consume(b',') {
            break;
        }
    }
    reader.expect(b'}')?;

    // extern-shaped names carry a `__`; `Event_` names are the built-in
    // events; the rest are graph-editor nodes (Branch, Const_…, Type_…) that
    // codegen never emits — but the `Type_` ones do tell us which types the
    // heap may declare.
    if full_name.contains(".__") {
        nodes.externs.insert(
            full_name,
            ExternNode {
                parameters: parameters.iter().map(|parameter| parameter.kind).collect(),
            },
        );
    } else if let Some(event_name) = full_name.strip_prefix("Event_") {
        // Only what the *runtime* raises. The Custom family is the graph's
        // SendCustomEvent node, and an event with an IN parameter is a node
        // configuration (OnVariableChange's variable name), not a value the
        // runtime hands to the program.
        let runtime_event = event_name != "Custom"
            && !event_name.starts_with("Custom_")
            && parameters
                .iter()
                .all(|parameter| parameter.kind == ParameterKind::Out);
        if runtime_event {
            let parameters = parameters
                .into_iter()
                .filter_map(|parameter| match (parameter.name, parameter.dotnet_type) {
                    (Some(name), Some(dotnet_type)) => Some(EventParameter { name, dotnet_type }),
                    _ => None,
                })
                .collect();
            nodes
                .events
                .insert(event_name.to_string(), EventNode { parameters });
        }
    } else if let Some(type_name) = full_name.strip_prefix("Type_") {
        nodes.types.insert(type_name.to_string());
    }
    Ok(())
}

struct ParsedParameter {
    name: Option<String>,
    dotnet_type: Option<String>,
    kind: ParameterKind,
}

fn parse_parameter(reader: &mut Reader, keep_names: bool) -> Parsed<ParsedParameter> {
    let mut parameter = ParsedParameter {
        name: None,
        dotnet_type: None,
        kind: ParameterKind::In,
    };
    reader.expect(b'{')?;
    loop {
        let key = reader.key()?;
        reader.expect(b':')?;
        match key {
            "kind" => {
                parameter.kind = match reader.key()? {
                    "IN" => ParameterKind::In,
                    "OUT" => ParameterKind::Out,
                    "IN_OUT" => ParameterKind::InOut,
                    other => {
                        return Err(reader.error(format!("unexpected parameter kind {other:?}")));
                    }
                };
            }
            // either may be the literal `null`
            "name" => {
                if reader.consume_null() {
                } else if keep_names {
                    parameter.name = Some(reader.string()?);
                } else {
                    reader.skip_string()?;
                }
            }
            "type" => {
                if reader.consume_null() {
                } else if keep_names {
                    parameter.dotnet_type = Some(reader.string()?);
                } else {
                    reader.skip_string()?;
                }
            }
            other => return Err(reader.error(format!("unexpected parameter key {other:?}"))),
        }
        if !reader.consume(b',') {
            break;
        }
    }
    reader.expect(b'}')?;
    Ok(parameter)
}

struct Reader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn error(&self, message: String) -> DumpError {
        DumpError {
            offset: Some(self.at),
            message,
        }
    }

    /// The byte under the cursor; running out of input is an error (a
    /// truncated file), never a panic.
    fn peek(&self) -> Parsed<u8> {
        self.bytes
            .get(self.at)
            .copied()
            .ok_or_else(|| self.error("unexpected end of the dump".to_string()))
    }

    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| b.is_ascii_whitespace())
        {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Parsed<()> {
        self.skip_whitespace();
        if self.peek()? != byte {
            return Err(self.error(format!("expected {:?}", byte as char)));
        }
        self.at += 1;
        Ok(())
    }

    fn consume(&mut self, byte: u8) -> bool {
        self.skip_whitespace();
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn consume_null(&mut self) -> bool {
        self.skip_whitespace();
        if self.bytes[self.at..].starts_with(b"null") {
            self.at += 4;
            true
        } else {
            false
        }
    }

    /// The end of a string run: the next quote or backslash.
    fn run_end(&mut self) -> Parsed<()> {
        while !matches!(self.peek()?, b'"' | b'\\') {
            self.at += 1;
        }
        Ok(())
    }

    /// A string that is only looked at, never kept: an object key, an enum-like
    /// value. Borrowed from the input, so it must not contain escapes (none of
    /// the dump's keys do; `string` handles the values that might).
    fn key(&mut self) -> Parsed<&'a str> {
        self.expect(b'"')?;
        let start = self.at;
        self.run_end()?;
        if self.bytes[self.at] != b'"' {
            return Err(self.error("escape in a key".to_string()));
        }
        self.at += 1;
        // both ends sit on ASCII quotes, so this is a char boundary
        Ok(&self.text[start..self.at - 1])
    }

    /// Skips a string value nothing needs.
    fn skip_string(&mut self) -> Parsed<()> {
        self.expect(b'"')?;
        loop {
            match self.peek()? {
                b'"' => {
                    self.at += 1;
                    return Ok(());
                }
                b'\\' => self.at += 2,
                _ => self.at += 1,
            }
        }
    }

    fn string(&mut self) -> Parsed<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            match self.peek()? {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.at += 1;
                    let escaped = self.peek()?;
                    self.at += 1;
                    match escaped {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self
                                .bytes
                                .get(self.at..self.at + 4)
                                .and_then(|hex| std::str::from_utf8(hex).ok())
                                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                                .ok_or_else(|| self.error("bad \\u escape".to_string()))?;
                            self.at += 4;
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        other => {
                            return Err(
                                self.error(format!("unexpected escape \\{}", other as char))
                            );
                        }
                    }
                }
                _ => {
                    // copy a UTF-8 run without per-char decoding
                    let start = self.at;
                    self.run_end()?;
                    // delimited by ASCII bytes on both sides: a char boundary
                    out.push_str(&self.text[start..self.at]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_dump_parses_and_indexes() {
        let nodes = UdonNodes::for_unity_version(None);
        assert_eq!(nodes.unity_version, "2022.3.22f1");
        assert_eq!(nodes.sdk, None);
        assert!(nodes.extern_count() > 30_000);

        // the signatures the code generator leans on hardest
        for signature in [
            "SystemInt32.__op_Addition__SystemInt32_SystemInt32__SystemInt32",
            "SystemObjectArray.__ctor__SystemInt32__SystemObjectArray",
            "SystemObjectArray.__Get__SystemInt32__SystemObject",
            "SystemObjectArray.__Set__SystemInt32_SystemObject__SystemVoid",
            "SystemArray.__Copy__SystemArray_SystemArray_SystemInt32__SystemVoid",
            "SystemString.__Concat__SystemString_SystemString__SystemString",
            "UnityEngineDebug.__Log__SystemObject__SystemVoid",
        ] {
            assert!(nodes.has_signature(signature), "missing {signature}");
        }

        // parameter shapes: instance + index + out
        let get = nodes
            .extern_node("SystemObjectArray.__Get__SystemInt32__SystemObject")
            .unwrap();
        assert_eq!(
            get.parameters,
            vec![ParameterKind::In, ParameterKind::In, ParameterKind::Out]
        );

        assert!(nodes.has_type("SystemInt32"));
        assert!(nodes.has_type("SystemObjectArray"));
    }

    #[test]
    fn version_selection_falls_back_to_newest() {
        let exact = UdonNodes::for_unity_version(Some("2022.3.22f1"));
        let fallback = UdonNodes::for_unity_version(Some("9999.9.9f9"));
        assert!(std::ptr::eq(exact, fallback));
    }

    /// A project's dump: the SDK it names, and an extern the embedded
    /// whitelist does not have (what a beta SDK brings).
    const PROJECT_DUMP: &str = "\u{feff}{
  \"unityVersion\": \"2022.3.22f1\",
  \"sdk\": \"com.vrchat.worlds 3.99.0-beta.1\",
  \"nodes\": [
    {\"fullName\": \"VRCSDK3BetaThing.__Ping__SystemString__SystemVoid\", \"relatedType\": null, \"parameters\": [{\"name\": \"message\", \"type\": \"System.String\", \"kind\": \"IN\"}]},
    {\"fullName\": \"Event_OnBetaThing\", \"relatedType\": \"null\", \"parameters\": [{\"name\": \"value\", \"type\": \"System.Int32\", \"kind\": \"OUT\"}]},
    {\"fullName\": \"Type_SystemInt32\", \"relatedType\": \"System.Int32\", \"parameters\": []}
  ]
}
";

    #[test]
    fn a_project_dump_parses_with_its_sdk() {
        let nodes = UdonNodes::parse(PROJECT_DUMP).unwrap();
        assert_eq!(
            nodes.sdk.as_deref(),
            Some("com.vrchat.worlds 3.99.0-beta.1")
        );
        assert!(nodes.has_signature("VRCSDK3BetaThing.__Ping__SystemString__SystemVoid"));
        assert!(!nodes.has_signature("UnityEngineDebug.__Log__SystemObject__SystemVoid"));
        assert_eq!(
            nodes.event("OnBetaThing").unwrap().parameters[0].name,
            "value"
        );
        assert!(nodes.has_type("SystemInt32"));
    }

    #[test]
    fn a_broken_dump_is_an_error_not_a_panic() {
        // cut short anywhere — a dump half written when the editor died
        for end in 0..PROJECT_DUMP.len() {
            if PROJECT_DUMP.is_char_boundary(end) && !PROJECT_DUMP[end..].trim().is_empty() {
                assert!(
                    UdonNodes::parse(&PROJECT_DUMP[..end]).is_err(),
                    "accepted a dump cut at byte {end}"
                );
            }
        }
        for (json, message) in [
            ("[]", "expected '{'"),
            ("{\"nodes\": [], \"what\": 1}", "unexpected key \"what\""),
            (
                "{\"unityVersion\": \"x\", \"nodes\": []}",
                "the dump lists no externs",
            ),
            ("{\"nodes\": []} trailing", "text after the end of the dump"),
            (
                "{\"nodes\": [{\"fullName\": \"A.__B__SystemVoid\", \"parameters\": [{\"kind\": \"SIDEWAYS\"}]}]}",
                "unexpected parameter kind \"SIDEWAYS\"",
            ),
        ] {
            let error = UdonNodes::parse(json).err().expect(json);
            assert!(
                error.message.contains(message),
                "{json}: {error} (wanted {message})"
            );
        }
    }

    #[test]
    fn dotnet_names_mangle_to_udon_names() {
        assert_eq!(mangle_dotnet_name("System.Int32"), "SystemInt32");
        assert_eq!(
            mangle_dotnet_name("UnityEngine.ParticleSystem+ExternalForcesModule"),
            "UnityEngineParticleSystemExternalForcesModule"
        );
        assert_eq!(
            mangle_dotnet_name("System.Collections.Generic.List`1"),
            "SystemCollectionsGenericList"
        );
        // generic instantiation = definition + arguments, composed structurally
        let list_of_int = format!(
            "{}{}",
            mangle_dotnet_name("System.Collections.Generic.List`1"),
            mangle_dotnet_name("System.Int32")
        );
        assert_eq!(list_of_int, "SystemCollectionsGenericListSystemInt32");
        assert!(UdonNodes::for_unity_version(None).has_type(&list_of_int));
    }
}
