use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser;
use futures_util::{StreamExt, stream};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use takeout_dl::cookies;
use takeout_dl::download::{Downloader, Outcome};
use takeout_dl::urls::{self, Part};
use takeout_dl::verify;

/// Download Google Takeout archives with resume, retries and parallel parts.
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    /// Takeout download URL(s). Quote them: they contain `&`.
    urls: Vec<String>,

    /// File with one URL per line (blank lines and `#` comments ignored).
    #[arg(long)]
    url_file: Option<PathBuf>,

    /// Total number of parts; each URL is expanded to this many parts starting at its own.
    #[arg(short, long)]
    parts: Option<u32>,

    /// Netscape-format cookies.txt exported from a browser logged in to Google.
    #[arg(short, long, required_unless_present = "dry_run")]
    cookies: Option<PathBuf>,

    /// Directory to save archives into.
    #[arg(short, long, default_value = ".")]
    out_dir: PathBuf,

    /// Parts to download concurrently.
    #[arg(short, long, default_value_t = 2)]
    jobs: usize,

    /// Retries per part after failures that made no progress.
    #[arg(long, default_value_t = 5)]
    retries: u32,

    /// Check each downloaded .tgz part end to end (gzip CRC) before giving it its final name.
    #[arg(long)]
    verify: bool,

    /// Print the parts that would be downloaded and exit.
    #[arg(long)]
    dry_run: bool,
}

fn collect_parts(args: &Args) -> Result<Vec<Part>> {
    let mut raw = args.urls.clone();
    if let Some(path) = &args.url_file {
        let contents =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        raw.extend(
            contents
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from),
        );
    }
    if raw.is_empty() {
        bail!("no URLs given (pass them as arguments or via --url-file)");
    }

    let mut seen = HashSet::new();
    let mut parts = Vec::new();
    for url in &raw {
        for part in urls::expand(url, args.parts)? {
            if seen.insert(part.filename.clone()) {
                parts.push(part);
            }
        }
    }
    Ok(parts)
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "off".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let parts = collect_parts(&args)?;

    if args.dry_run {
        for p in &parts {
            println!("{}\t{}", p.filename, p.url);
        }
        return Ok(ExitCode::SUCCESS);
    }

    let cookie_path = args.cookies.as_ref().expect("clap requires --cookies");
    let jar = cookies::load(cookie_path)?;
    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("creating {}", args.out_dir.display()))?;
    let downloader =
        Downloader::new(jar, args.out_dir.clone(), args.retries)?.with_verify(args.verify);
    let unverifiable = parts
        .iter()
        .filter(|p| !verify::supported(&p.filename))
        .count();
    if args.verify && unverifiable > 0 {
        eprintln!("note: --verify only checks .tgz parts; {unverifiable} part(s) won't be checked");
    }

    let multi = MultiProgress::new();
    let style = ProgressStyle::with_template(
        "{prefix:.bold} [{bar:30.cyan/blue}] {bytes}/{total_bytes} {binary_bytes_per_sec} eta {eta} {msg}",
    )?
    .progress_chars("=> ");
    let overall = multi.add(ProgressBar::new(parts.len() as u64));
    overall.set_style(ProgressStyle::with_template("{pos}/{len} parts done")?);

    let results: Vec<(String, Result<Outcome, _>)> = stream::iter(&parts)
        .map(|part| {
            let pb = multi.add(ProgressBar::new(0).with_style(style.clone()));
            pb.set_prefix(part.filename.clone());
            let (downloader, overall) = (&downloader, &overall);
            async move {
                let result = downloader.download(part, &pb).await;
                pb.finish_and_clear();
                overall.inc(1);
                (part.filename.clone(), result)
            }
        })
        .buffer_unordered(args.jobs.max(1))
        .collect()
        .await;
    overall.finish_and_clear();

    let mut failed = 0;
    for (name, result) in &results {
        match result {
            Ok(Outcome::Downloaded) => println!("ok       {name}"),
            Ok(Outcome::AlreadyPresent) => println!("skipped  {name} (already exists)"),
            Err(e) => {
                failed += 1;
                println!("FAILED   {name}: {e}");
            }
        }
    }
    println!(
        "{} of {} parts complete",
        results.len() - failed,
        results.len()
    );
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
