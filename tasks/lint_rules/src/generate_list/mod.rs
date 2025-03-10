use std::collections::HashSet;
use std::{fs::read_dir, path::Path};
use ureq::Response;

mod eslint;

pub fn run(plugin_name: &str) -> Result<String, String> {
    let (js_source_url, find_to_be_implemented_rules, our_rules_dir) = match plugin_name {
        "eslint" => (
            eslint::ORIGINAL_JS_SOURCE_URL,
            eslint::find_to_be_implemented_rules,
            eslint::OUR_RULES_DIR,
        ),
        _ => return Err(format!("😢 Unknown plugin name: {plugin_name}")),
    };

    let js_string = fetch_plugin_rules_js_string(js_source_url)?;
    let rules_to_be_implemented = find_to_be_implemented_rules(&js_string)?;
    let rules_implemented = list_implemented_rules(Path::new(our_rules_dir))?;

    let list = render_markdown_todo_list(&rules_to_be_implemented, &rules_implemented);
    Ok(list)
}

fn fetch_plugin_rules_js_string(url: &str) -> Result<String, String> {
    let body = oxc_tasks_common::agent().get(url).call().map(Response::into_string);

    match body {
        Ok(Ok(body)) => Ok(body),
        Ok(Err(err)) => Err(err.to_string()),
        Err(err) => Err(err.to_string()),
    }
}

fn list_implemented_rules(path: &Path) -> Result<Vec<String>, String> {
    let entries = match read_dir(path) {
        Ok(entries) => entries,
        Err(err) => return Err(err.to_string()),
    };

    let mut rules = vec![];
    for entry in entries.flatten() {
        // This is file or directory
        let os_str = entry.file_name();
        let name = os_str.to_string_lossy().trim_end_matches(".rs").replace('_', "-");
        rules.push(name);
    }

    Ok(rules)
}

fn render_markdown_todo_list(theirs: &[String], ours: &[String]) -> String {
    let mut ours = ours.iter().collect::<HashSet<_>>();

    let mut list = vec![];
    for rule in theirs {
        let mark = if ours.remove(rule) { "x" } else { " " };
        list.push(format!("- [{mark}] {rule}"));
    }

    for rule in &ours {
        eprintln!("⚠️ Rule: {rule} is implemented but not in their lists.");
    }

    list.join("\n")
}
