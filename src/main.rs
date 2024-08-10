// Copyright (c) 2024 Jan Holthuis <jan.holthuis@rub.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy
// of the MPL was not distributed with this file, You can obtain one at
// http://mozilla.org/MPL/2.0/.
//
// SPDX-License-Identifier: MPL-2.0
use clap::Parser;
use log::{info, warn};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Converter for playlists into a Ford Sync 2 compatible format.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to write output to.
    #[arg(short, long, default_value = "output")]
    output_dir: PathBuf,

    /// Playlist files
    #[arg(num_args=1..)]
    playlists: Vec<PathBuf>,
}

fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        // Set `RUST_LOG=myapp=debug` to see debug logs, this only shows info logs.
        std::env::set_var("RUST_LOG", "info");
    }
    pretty_env_logger::init();

    let args = Cli::parse();

    std::fs::create_dir_all(&args.output_dir).unwrap();

    let mut files_to_copy = vec![];
    let mut files_to_convert = vec![];
    for input_playlist_path in args.playlists.iter() {
        info!("Parsing Playlist: {}", input_playlist_path.display());
        let mut reader = m3u::Reader::open(input_playlist_path).unwrap();

        let output_playlist_filename = input_playlist_path.file_name().unwrap();
        let output_playlist_path = args.output_dir.as_path().join(output_playlist_filename);
        let mut output_playlist_file = std::fs::File::create(&output_playlist_path).unwrap();
        let mut writer = m3u::Writer::new(&mut output_playlist_file);

        let input_playlist_dir = input_playlist_path
            .parent()
            .map(|parent| {
                if parent == Path::new("") {
                    PathBuf::from(".")
                } else {
                    parent.to_path_buf()
                }
            })
            .unwrap();
        for input_audio_path in reader
            .entries()
            .filter_map(|res| match res {
                Ok(entry) => Some(entry),
                Err(e) => {
                    warn!("Failed to read playlist entry: {}", e);
                    None
                }
            })
            .filter_map(|entry| match entry {
                m3u::Entry::Path(path) => Some(path),
                m3u::Entry::Url(url) => {
                    warn!("Ignoring URL: {}", url);
                    None
                }
            })
        {
            let extension = match input_audio_path.extension() {
                Some(ext) => ext,
                None => {
                    warn!("{}: Failed to determine file extension", input_audio_path.display());
                    continue
                },
            };
            let output_audio_path = if extension == "mp3" {
                files_to_copy.push((
                    input_playlist_dir.join(&input_audio_path),
                    args.output_dir.join(&input_audio_path),
                ));
                input_audio_path
            } else {
                let new_audio_path = input_audio_path.with_extension("mp3");
                files_to_convert.push((
                    input_playlist_dir.join(&input_audio_path),
                    args.output_dir.join(&new_audio_path),
                ));
                new_audio_path
            };

            // Write windows path to file
            let mut output_audio_path_windows =
                output_audio_path.iter().fold(String::new(), |mut a, b| {
                    let b_str = b.to_str().unwrap();
                    a.reserve(b_str.len() + 1);
                    a.push_str(b_str);
                    a.push('\\');
                    a
                });
            output_audio_path_windows.truncate(output_audio_path_windows.len() - 1);
            writer
                .write_entry(&m3u::path_entry(&output_audio_path_windows))
                .unwrap();
        }
        writer.flush().unwrap();
        info!("Wrote Playlist: {}", output_playlist_path.display());
    }

    info!("Files to copy: {}", files_to_copy.len());
    info!("Files to convert: {}", files_to_convert.len());

    let num_copy_tasks = files_to_copy.len();
    let num_convert_tasks = files_to_convert.len();
    let num_tasks_total = num_copy_tasks + num_convert_tasks;

    info!("Starting convert files...");
    use std::sync::mpsc::channel;
    use threadpool::ThreadPool;

    let n_workers = 4;
    let pool = ThreadPool::new(n_workers);

    let (tx, rx) = channel();
    for (input_path, output_path) in files_to_convert.into_iter() {
        let input_path_str = input_path.into_os_string().into_string().unwrap();
        let output_path_str = output_path.clone().into_os_string().into_string().unwrap();
        let output_dir = output_path.parent().unwrap();
        std::fs::create_dir_all(output_dir).unwrap();

        let tx = tx.clone();
        pool.execute(move || {
            let output = Command::new("ffmpeg")
                .args([
                    "-i",
                    &input_path_str,
                    "-y",
                    "-vn",
                    "-aq",
                    "2",
                    &output_path_str,
                ])
                .output();
            tx.send((output_path, output))
                .expect("channel will be there waiting for the pool");
        });
    }

    for (i, (output_path, result)) in rx.iter().take(num_convert_tasks).enumerate() {
        let index = i + 1;
        match result {
            Ok(output) => {
                if output.status.success() {
                    info!(
                        "({}/{}) {}: Conversion succeeded.",
                        index,
                        num_tasks_total,
                        output_path.display()
                    );
                } else {
                    warn!(
                        "({}/{}) {}: FFmpeg exited with non-zero status {}",
                        index,
                        num_tasks_total,
                        output_path.display(),
                        output.status
                    );
                }
            }
            Err(e) => {
                warn!(
                    "({}/{}) {}: Failed to execute FFmpeg ({})",
                    index,
                    num_tasks_total,
                    output_path.display(),
                    e
                );
            }
        }
    }

    info!("Starting to copy files...");
    for (i, (input_path, output_path)) in files_to_copy.into_iter().enumerate() {
        let index = i + num_convert_tasks + 1;
        let output_dir = output_path.parent().unwrap();
        std::fs::create_dir_all(output_dir).unwrap();
        let result = std::fs::copy(input_path, &output_path);
        match result {
            Ok(_) => {
                info!(
                    "({}/{}) {}: Copying succeeded.",
                    index,
                    num_tasks_total,
                    output_path.display()
                );
            }
            Err(e) => {
                warn!(
                    "({}/{}) {}: Failed to copy file ({})",
                    index,
                    num_tasks_total,
                    output_path.display(),
                    e
                );
            }
        }
    }
}
