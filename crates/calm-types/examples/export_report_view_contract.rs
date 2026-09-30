use calm_types::report_blocks::native_view;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--schema") => println!(
            "{}",
            serde_json::to_string_pretty(&native_view::generated_schema()).unwrap()
        ),
        Some("--types") => print!("{}", native_view::typescript()),
        _ => {
            eprintln!("usage: export_report_view_contract --schema|--types");
            std::process::exit(2);
        }
    }
}
