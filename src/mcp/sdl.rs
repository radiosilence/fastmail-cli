//! Slicing the GraphQL SDL down to the types a caller actually asked for.
//!
//! The full SDL is ~27KB, most of it doc comments — which are worth having, and
//! are exactly what makes it expensive. A session that only sends mail pays for
//! the whole contact and masked-email surface to find the one mutation it
//! needs, and pays again every time the connection is re-established.
//!
//! Text slicing rather than re-rendering from `async_graphql`'s registry: the
//! registry exposes no per-type printer, and hand-rolling one would drift from
//! whatever `Schema::sdl()` emits. Splitting the emitted SDL cannot drift,
//! because it *is* the emitted SDL.

/// One top-level definition and the lines it spans, doc comment included.
struct Definition<'a> {
    name: &'a str,
    span: std::ops::Range<usize>,
}

/// The name a top-level definition declares, or `None` for a line that starts
/// no definition.
fn definition_name(header: &str) -> Option<&str> {
    let mut words = header.split_whitespace();
    match words.next()? {
        "type" | "input" | "enum" | "scalar" | "union" | "interface" => {
            words.next().map(|n| n.trim_end_matches('{'))
        }
        // Addressable under the name you'd write in a query.
        "directive" => words.next().map(|n| n.split('(').next().unwrap_or(n)),
        "schema" => Some("schema"),
        _ => None,
    }
}

/// Split SDL into its top-level definitions, in the order they appear.
///
/// Relies only on layout `Schema::sdl()` guarantees: definitions begin in column
/// zero, their bodies are indented, a block closes on a column-zero `}`, and a
/// doc comment sits immediately above the definition it describes.
fn definitions<'a>(lines: &[&'a str]) -> Vec<Definition<'a>> {
    let mut defs = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let start = i;

        // A column-zero doc block belongs to whatever follows it.
        if lines[i] == "\"\"\"" {
            i += 1;
            while i < lines.len() && lines[i] != "\"\"\"" {
                i += 1;
            }
            i += 1;
        }
        let Some(header) = lines.get(i) else { break };

        let name = definition_name(header);
        if header.ends_with('{') {
            i += 1;
            while i < lines.len() && lines[i] != "}" {
                i += 1;
            }
        }
        i += 1;

        if let Some(name) = name {
            defs.push(Definition {
                name,
                span: start..i,
            });
        }
    }

    defs
}

/// Every type name in the schema, in SDL order. `slice` reports these itself
/// when a name matches nothing; this exists so the tests can enumerate them.
#[cfg(test)]
pub fn type_names(sdl: &str) -> Vec<String> {
    let lines: Vec<&str> = sdl.lines().collect();
    definitions(&lines)
        .iter()
        .map(|d| d.name.to_string())
        .collect()
}

/// The definitions named in `wanted`, in schema order.
///
/// Matching is case-insensitive as a fallback: a model reaching for `session`
/// means `Session`, and a round trip to be told so helps nobody.
///
/// Names that match nothing are reported in a trailing SDL comment along with
/// the full list of type names — the output stays valid SDL, and a typo doesn't
/// silently return a schema with a hole in it.
pub fn slice(sdl: &str, wanted: &[String]) -> String {
    let lines: Vec<&str> = sdl.lines().collect();
    let defs = definitions(&lines);

    let resolve = |want: &str| {
        defs.iter()
            .find(|d| d.name == want)
            .or_else(|| defs.iter().find(|d| d.name.eq_ignore_ascii_case(want)))
    };

    let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
    let mut unknown: Vec<&str> = Vec::new();
    for want in wanted {
        match resolve(want) {
            // Duplicates in `wanted` shouldn't duplicate the output.
            Some(def) if !spans.contains(&def.span) => spans.push(def.span.clone()),
            Some(_) => {}
            None => unknown.push(want),
        }
    }
    // Schema order, not request order: the SDL reads as a schema either way, and
    // this keeps the output stable across differently-ordered requests.
    spans.sort_by_key(|s| s.start);

    let mut out: Vec<String> = spans
        .into_iter()
        .map(|span| lines[span].join("\n"))
        .collect();

    if !unknown.is_empty() {
        out.push(format!(
            "# No such type: {}.\n# The schema defines: {}.",
            unknown.join(", "),
            defs.iter().map(|d| d.name).collect::<Vec<_>>().join(", ")
        ));
    }

    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::graphql::build_schema;

    fn sdl() -> String {
        build_schema().sdl()
    }

    #[test]
    fn slices_out_one_type_with_its_doc_comment() {
        let out = slice(&sdl(), &["Session".into()]);

        assert!(
            out.starts_with("\"\"\""),
            "doc comment must come along: {out}"
        );
        assert!(out.contains("Who the token authenticates as"));
        assert!(out.contains("type Session {"));
        assert!(out.trim_end().ends_with('}'), "block must be closed: {out}");
        // The point of the whole exercise.
        assert!(
            !out.contains("type Email "),
            "sliced SDL leaked other types"
        );
    }

    #[test]
    fn a_slice_is_a_small_fraction_of_the_whole() {
        let full = sdl();
        let out = slice(&full, &["Session".into(), "ConnectionStatus".into()]);
        assert!(
            out.len() * 10 < full.len(),
            "expected a big saving, got {} of {}",
            out.len(),
            full.len()
        );
    }

    #[test]
    fn returns_definitions_in_schema_order_however_they_were_asked_for() {
        let full = sdl();
        let forwards = slice(&full, &["Account".into(), "Session".into()]);
        let backwards = slice(&full, &["Session".into(), "Account".into()]);
        assert_eq!(forwards, backwards);
        assert!(forwards.find("type Account").unwrap() < forwards.find("type Session").unwrap());
    }

    #[test]
    fn handles_inputs_enums_and_the_roots() {
        for name in [
            "EmailFilter",
            "EmailSortProperty",
            "QueryRoot",
            "MutationRoot",
        ] {
            let out = slice(&sdl(), &[name.into()]);
            assert!(
                out.contains(&format!(" {name} {{")),
                "missing {name}: {out}"
            );
            assert!(out.trim_end().ends_with('}'), "unterminated {name}");
        }
    }

    #[test]
    fn a_repeated_name_is_returned_once() {
        let out = slice(&sdl(), &["Session".into(), "Session".into()]);
        assert_eq!(out.matches("type Session {").count(), 1);
    }

    #[test]
    fn a_wrong_case_name_still_resolves() {
        assert!(slice(&sdl(), &["session".into()]).contains("type Session {"));
    }

    #[test]
    fn an_unknown_name_is_reported_rather_than_silently_dropped() {
        let out = slice(&sdl(), &["Session".into(), "Emial".into()]);

        assert!(
            out.contains("type Session {"),
            "the valid half must survive"
        );
        assert!(out.contains("# No such type: Emial."), "got {out}");
        // The recovery path: the names it could have meant.
        assert!(out.contains("Email"));
    }

    #[test]
    fn every_definition_in_the_schema_is_addressable() {
        let full = sdl();
        for name in type_names(&full) {
            let out = slice(&full, std::slice::from_ref(&name));
            assert!(
                !out.contains("# No such type"),
                "{name} is in the schema but not addressable"
            );
        }
    }

    #[test]
    fn asking_for_everything_reconstructs_the_schema() {
        let full = sdl();
        let out = slice(&full, &type_names(&full));
        // Not byte-identical — blank-line runs between definitions collapse —
        // but every definition must be present and whole.
        for line in full.lines().filter(|l| definition_name(l).is_some()) {
            assert!(out.contains(line), "lost: {line}");
        }
    }
}
