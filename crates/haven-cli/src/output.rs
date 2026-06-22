//! Output rendering. JSON to stdout by default (one value per invocation);
//! `--pretty` renders human tables for the common shapes. Errors go to stderr
//! as `{"error": {...}}` with a non-zero exit (SPEC §2).

use haven_core::{AddOutcome, HavenError, Item, Project};
use serde_json::Value;

/// The result of a command, tagged so `--pretty` can render a nice table while
/// the default path just serializes to JSON.
// Variants differ in size (an `Item` is large); this value is short-lived and
// constructed once per CLI invocation, so boxing would only add indirection.
#[allow(clippy::large_enum_variant)]
pub enum Output {
    Item(Item),
    AddOutcome(AddOutcome),
    Items(Vec<Item>),
    Project(Project),
    Projects(Vec<Project>),
    Json(Value),
    Message(String),
    /// A pre-rendered text block emitted verbatim in BOTH default and `--pretty`
    /// modes — the block IS the payload (e.g. `haven prime`), not a JSON value.
    Text(String),
    Unit,
}

impl Output {
    pub fn render(&self, pretty: bool) {
        // A pre-rendered block is the payload itself — emit it verbatim in both
        // modes (no JSON wrapping, no table), so `haven prime` is a clean block.
        if let Output::Text(block) = self {
            print!("{block}");
            return;
        }
        if pretty {
            self.render_pretty();
        } else {
            println!("{}", self.to_json_string());
        }
    }

    fn to_json_string(&self) -> String {
        let value = self.to_json();
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "null".into())
    }

    fn to_json(&self) -> Value {
        match self {
            Output::Item(i) => serde_json::to_value(i).unwrap_or(Value::Null),
            Output::AddOutcome(o) => serde_json::to_value(o).unwrap_or(Value::Null),
            Output::Items(v) => serde_json::to_value(v).unwrap_or(Value::Null),
            Output::Project(p) => serde_json::to_value(p).unwrap_or(Value::Null),
            Output::Projects(v) => serde_json::to_value(v).unwrap_or(Value::Null),
            Output::Json(v) => v.clone(),
            Output::Message(m) => serde_json::json!({ "message": m }),
            // Handled verbatim in `render`; this arm is only for completeness.
            Output::Text(t) => serde_json::json!({ "text": t }),
            Output::Unit => serde_json::json!({ "ok": true }),
        }
    }

    fn render_pretty(&self) {
        match self {
            Output::Item(i) => print!("{}", item_table(std::slice::from_ref(i))),
            Output::AddOutcome(o) => {
                print!("{}", item_table(std::slice::from_ref(&o.item)));
                if o.existing {
                    println!("(existing item — nothing created)");
                }
                for s in &o.similar {
                    println!("similar: {}  {}", s.reference, s.title);
                }
            }
            Output::Items(v) => print!("{}", item_table(v)),
            Output::Project(p) => print!("{}", project_table(std::slice::from_ref(p))),
            Output::Projects(v) => print!("{}", project_table(v)),
            Output::Message(m) => println!("{m}"),
            // Verbatim block — already intercepted in `render`; kept exhaustive.
            Output::Text(t) => print!("{t}"),
            Output::Unit => println!("ok"),
            Output::Json(v) => println!("{}", serde_json::to_string_pretty(v).unwrap_or_default()),
        }
    }
}

fn item_table(items: &[Item]) -> String {
    if items.is_empty() {
        return "(no items)\n".into();
    }
    let mut rows: Vec<[String; 6]> = vec![[
        "REF".into(),
        "TITLE".into(),
        "STATUS".into(),
        "OWNER".into(),
        "PRI".into(),
        "C".into(),
    ]];
    for i in items {
        rows.push([
            i.reference.clone(),
            truncate(&i.title, 50),
            i.status.as_str().into(),
            i.owner_kind
                .map(|o| o.as_str().to_string())
                .unwrap_or_default(),
            i.priority.map(|p| p.to_string()).unwrap_or_default(),
            if i.committed { "*".into() } else { "".into() },
        ]);
    }
    table(&rows)
}

fn project_table(projects: &[Project]) -> String {
    if projects.is_empty() {
        return "(no projects)\n".into();
    }
    let mut rows: Vec<[String; 5]> = vec![[
        "KEY".into(),
        "PREFIX".into(),
        "TITLE".into(),
        "ITEMS#".into(),
        "STATUS".into(),
    ]];
    for p in projects {
        rows.push([
            p.key.clone(),
            p.ref_prefix.clone(),
            truncate(&p.title, 40),
            // `ITEMS#` is `ref_counter`, the preserved namespace counter — under
            // archive this is exactly what stays reserved (HV-123).
            p.ref_counter.to_string(),
            p.status.as_str().to_string(),
        ]);
    }
    table(&rows)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Render a fixed-width column table from rows where row 0 is the header.
fn table<const N: usize>(rows: &[[String; N]]) -> String {
    let mut widths = [0usize; N];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for (r, row) in rows.iter().enumerate() {
        for (i, cell) in row.iter().enumerate() {
            let pad = widths[i] - cell.chars().count();
            out.push_str(cell);
            if i + 1 < N {
                out.push_str(&" ".repeat(pad + 2));
            }
        }
        out.push('\n');
        if r == 0 {
            // underline header
            let total: usize = widths.iter().sum::<usize>() + 2 * (N - 1);
            out.push_str(&"-".repeat(total));
            out.push('\n');
        }
    }
    out
}

/// Print the error envelope to stderr and return the process exit code.
pub fn render_error(err: &HavenError) -> i32 {
    let envelope = serde_json::json!({
        "error": { "code": err.code(), "message": err.to_string() }
    });
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&envelope).unwrap_or_default()
    );
    1
}
