use aicleaner_lib::log_cli_support::{
    aggregate_runs, build_ai_task_package, discover_log_files, find_file_by_path, newest_run,
    parse_family, read_records, read_records_lossy, resolve_paths, serialize_ai_package_jsonl,
    summarize_runs, LogFamily, RecordFilter, ResolveOptions,
};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

const DEFAULT_AI_LIMIT: usize = 3;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorEnvelope {
    error: String,
}

#[derive(Parser, Debug)]
#[command(name = "aicleaner-logs", version, about = "Unified AIcleaner log CLI")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    settings_path: Option<PathBuf>,
    #[arg(long, global = true)]
    logs_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Doctor,
    List(ListArgs),
    Show(ShowArgs),
    Summary(SummaryArgs),
    Export(ExportArgs),
    Request(RequestArgs),
}

#[derive(Args, Debug, Default)]
struct FilterArgs {
    #[arg(long)]
    family: Option<String>,
    #[arg(long)]
    level: Option<String>,
    #[arg(long)]
    event: Option<String>,
    #[arg(long)]
    task_id: Option<String>,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    job_id: Option<String>,
    #[arg(long)]
    operation_id: Option<String>,
    #[arg(long)]
    since: Option<String>,
}

#[derive(Args, Debug, Default)]
struct ListArgs {
    #[command(flatten)]
    filter: FilterArgs,
    #[arg(long, default_value_t = DEFAULT_AI_LIMIT)]
    limit: usize,
}

#[derive(Args, Debug)]
struct ShowArgs {
    #[command(flatten)]
    filter: FilterArgs,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long)]
    tail: Option<usize>,
    #[arg(long, default_value = "json")]
    format: String,
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct SummaryArgs {
    #[command(flatten)]
    filter: FilterArgs,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long, default_value_t = DEFAULT_AI_LIMIT)]
    limit: usize,
}

#[derive(Args, Debug)]
struct ExportArgs {
    #[command(flatten)]
    filter: FilterArgs,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long, default_value = "json")]
    format: String,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, default_value_t = DEFAULT_AI_LIMIT)]
    limit: usize,
}

#[derive(Args, Debug)]
struct RequestArgs {
    #[arg(long)]
    action: String,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long)]
    family: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    let json_mode = cli.json;
    if let Err(err) = run(cli) {
        if json_mode {
            let envelope = ErrorEnvelope {
                error: err.to_string(),
            };
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| {
                    "{\"error\":\"failed to encode error\"}".to_string()
                })
            );
        } else {
            eprintln!("{}", err);
        }
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let paths = resolve_paths(&ResolveOptions {
        data_dir: cli.data_dir,
        settings_path: cli.settings_path,
        logs_dir: cli.logs_dir,
    })?;
    let files = discover_log_files(&paths)?;

    match cli.command {
        Command::Doctor => print_output(
            cli.json,
            &serde_json::json!({
                "dataDir": paths.data_dir,
                "settingsPath": paths.settings_path,
                "logsDir": paths.logs_dir,
                "legacyAppLogsDir": paths.legacy_app_logs_dir,
                "availableFamilies": available_families(&files),
                "fileCount": files.len(),
            }),
        ),
        Command::List(args) => {
            let records = load_selected_records(&files, None, &args.filter)?;
            let runs = aggregate_runs(&records);
            let summaries = summarize_runs(&runs, args.limit.max(1));
            print_output(cli.json, &summaries)
        }
        Command::Show(args) => {
            let selected_files = select_files(&files, args.file, args.filter.family.as_deref())?;
            let (records, _) = read_records_lossy(&selected_files, &build_record_filter(&args.filter)?)?;
            let records = if let Some(tail) = args.tail {
                records
                    .into_iter()
                    .rev()
                    .take(tail)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            } else {
                records
            };
            let content = serialize_records(&records, &args.format)?;
            write_or_print_content(cli.json, &content, args.output)
        }
        Command::Summary(args) => {
            let (runs, parse_errors) =
                load_selected_runs_lossy(&files, args.file, &args.filter)?;
            let run = newest_run(&runs).ok_or_else(|| anyhow!("no matching runs found"))?;
            let package = build_ai_task_package(run, parse_errors.len(), args.limit.max(1));
            print_output(cli.json, &package)
        }
        Command::Export(args) => {
            let (runs, parse_errors) =
                load_selected_runs_lossy(&files, args.file, &args.filter)?;
            let run = newest_run(&runs).ok_or_else(|| anyhow!("no matching runs found"))?;
            let package = build_ai_task_package(run, parse_errors.len(), args.limit.max(1));
            let content = serialize_ai_package(&package, &args.format)?;
            if let Some(output) = args.output {
                fs::write(&output, &content)
                    .with_context(|| format!("failed to write {}", output.display()))?;
                print_output(
                    cli.json,
                    &serde_json::json!({
                        "written": true,
                        "output": output,
                        "id": package.id,
                        "idKind": package.id_kind,
                        "family": package.family,
                    }),
                )
            } else {
                println!("{content}");
                Ok(())
            }
        }
        Command::Request(args) => {
            let payload = match args.action.as_str() {
                "files" => serde_json::to_value(&files)?,
                "records" => {
                    let filter = FilterArgs {
                        family: args.family,
                        ..FilterArgs::default()
                    };
                    serde_json::to_value(load_selected_records(&files, args.file, &filter)?)?
                }
                other => return Err(anyhow!("unsupported request action '{}'", other)),
            };
            print_output(cli.json, &payload)
        }
    }
}

fn load_selected_records(
    files: &[aicleaner_lib::log_cli_support::LogFileInfo],
    file: Option<PathBuf>,
    filter: &FilterArgs,
) -> Result<Vec<aicleaner_lib::log_cli_support::ParsedRecord>> {
    let selected_files = select_files(files, file, filter.family.as_deref())?;
    read_records(&selected_files, &build_record_filter(filter)?)
}

fn load_selected_runs_lossy(
    files: &[aicleaner_lib::log_cli_support::LogFileInfo],
    file: Option<PathBuf>,
    filter: &FilterArgs,
) -> Result<(Vec<aicleaner_lib::log_cli_support::AggregatedRun>, Vec<String>)> {
    let selected_files = select_files(files, file, filter.family.as_deref())?;
    let (records, parse_errors) = read_records_lossy(&selected_files, &build_record_filter(filter)?)?;
    Ok((aggregate_runs(&records), parse_errors))
}

fn select_files(
    files: &[aicleaner_lib::log_cli_support::LogFileInfo],
    file: Option<PathBuf>,
    family: Option<&str>,
) -> Result<Vec<aicleaner_lib::log_cli_support::LogFileInfo>> {
    if let Some(path) = file {
        return Ok(vec![
            find_file_by_path(files, &path)
                .cloned()
                .ok_or_else(|| anyhow!("log file not found: {}", path.display()))?,
        ]);
    }

    let family = match family {
        Some(value) => Some(parse_family(value)?),
        None => None,
    };
    Ok(files
        .iter()
        .filter(|entry| family.as_ref().map(|item| &entry.family == item).unwrap_or(true))
        .cloned()
        .collect())
}

fn build_record_filter(args: &FilterArgs) -> Result<RecordFilter> {
    let family = match args.family.as_deref() {
        Some(value) => Some(parse_family(value)?),
        None => None,
    };
    let since = args
        .since
        .as_deref()
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|dt| dt.with_timezone(&Utc))
                .with_context(|| format!("invalid --since value '{}'", value))
        })
        .transpose()?;
    Ok(RecordFilter {
        family,
        level: args.level.clone(),
        event: args.event.clone(),
        task_id: args.task_id.clone(),
        session_id: args.session_id.clone(),
        job_id: args.job_id.clone(),
        operation_id: args.operation_id.clone(),
        since,
    })
}

fn available_families(files: &[aicleaner_lib::log_cli_support::LogFileInfo]) -> Vec<&'static str> {
    let mut families = Vec::new();
    if files.iter().any(|file| file.family == LogFamily::Diagnostics) {
        families.push("diagnostics");
    }
    if files.iter().any(|file| file.family == LogFamily::WebSearch) {
        families.push("web_search");
    }
    if files.iter().any(|file| file.family == LogFamily::App) {
        families.push("app");
    }
    families
}

fn serialize_records<T: Serialize>(records: &T, format: &str) -> Result<String> {
    match format {
        "json" => Ok(serde_json::to_string_pretty(records)?),
        "jsonl" => {
            let values = serde_json::to_value(records)?;
            let array = values
                .as_array()
                .ok_or_else(|| anyhow!("jsonl serialization requires an array"))?;
            Ok(array
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n"))
        }
        other => Err(anyhow!("unsupported show format '{}'", other)),
    }
}

fn serialize_ai_package(
    package: &aicleaner_lib::log_cli_support::AiTaskPackage,
    format: &str,
) -> Result<String> {
    match format {
        "json" => Ok(serde_json::to_string_pretty(package)?),
        "jsonl" => serialize_ai_package_jsonl(package),
        other => Err(anyhow!("unsupported export format '{}'", other)),
    }
}

fn write_or_print_content(as_json: bool, content: &str, output: Option<PathBuf>) -> Result<()> {
    if let Some(output) = output {
        fs::write(&output, content)
            .with_context(|| format!("failed to write {}", output.display()))?;
        print_output(
            as_json,
            &serde_json::json!({
                "written": true,
                "output": output,
            }),
        )
    } else {
        println!("{content}");
        Ok(())
    }
}

fn print_output<T: Serialize>(_: bool, value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
