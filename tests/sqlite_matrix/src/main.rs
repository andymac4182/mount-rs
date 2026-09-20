use mount_rs_sqlite_matrix::{run_matrix, summarize};

fn main() {
    let reports = run_matrix();
    for report in &reports {
        println!(
            "{}",
            serde_json::to_string(report).expect("case report must serialize")
        );
    }

    let summary = summarize(&reports);
    println!(
        "{}",
        serde_json::to_string(&summary).expect("summary must serialize")
    );
    if summary.failed != 0 {
        std::process::exit(1);
    }
}
