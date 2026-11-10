use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, header},
    response::Response,
};
use std::{
    fs,
    io::{self, Read, Seek, Write},
    path::Path,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{
    AppError, AppState,
    files::{Target, content_disposition},
};

const CHUNK_SIZE: usize = 64 * 1024;
type Chunk = Result<Bytes, io::Error>;

struct ChannelWriter(mpsc::Sender<Chunk>);

impl Write for ChannelWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for chunk in bytes.chunks(CHUNK_SIZE) {
            self.0
                .blocking_send(Ok(Bytes::copy_from_slice(chunk)))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "下载已取消"))?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn stream_archive(
    state: &AppState,
    targets: Vec<Target>,
    filename: &str,
) -> Result<Response, AppError> {
    let disposition = content_disposition("attachment", filename)?;
    let permit = state
        .archive_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::new(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "同时打包的下载过多，请稍后重试",
            )
        })?;
    let (sender, receiver) = mpsc::channel(4);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if let Err(error) = write_archive(&sender, targets)
            && !sender.is_closed()
        {
            eprintln!("归档生成失败：{error}");
            let _ = sender.blocking_send(Err(io::Error::other("归档生成失败")));
        }
    });
    let mut response = Response::new(Body::from_stream(ReceiverStream::new(receiver)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition);
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}

fn write_archive(
    sender: &mpsc::Sender<Chunk>,
    targets: Vec<Target>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut archive = ZipWriter::new_stream(ChannelWriter(sender.clone()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for target in targets {
        let name = target.relative.rsplit('/').next().unwrap().to_owned();
        if target.metadata.is_file() {
            append_file(&mut archive, &target.path, &name, sender)?;
            continue;
        }
        archive.add_directory(format!("{name}/"), options)?;
        let mut stack = vec![(fs::read_dir(&target.path)?, name)];
        while let Some((directory, prefix)) = stack.last_mut() {
            if sender.is_closed() {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "下载已取消").into());
            }
            let Some(entry) = directory.next() else {
                stack.pop();
                continue;
            };
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.is_file() && !metadata.is_dir() {
                continue;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.contains('\\') {
                continue;
            }
            let name = format!("{prefix}/{name}");
            if metadata.is_dir() {
                archive.add_directory(format!("{name}/"), options)?;
                stack.push((fs::read_dir(entry.path())?, name));
            } else {
                append_file(&mut archive, &entry.path(), &name, sender)?;
            }
        }
    }
    archive.finish()?;
    Ok(())
}

fn append_file<W: Write + Seek>(
    archive: &mut ZipWriter<W>,
    path: &Path,
    name: &str,
    sender: &mpsc::Sender<Chunk>,
) -> Result<(), Box<dyn std::error::Error>> {
    if sender.is_closed() {
        return Err(io::Error::new(io::ErrorKind::BrokenPipe, "下载已取消").into());
    }
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "目标必须是普通文件").into());
    }
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(metadata.len() >= u32::MAX as u64);
    archive.start_file(name, options)?;
    let mut buffer = [0_u8; CHUNK_SIZE];
    loop {
        if sender.is_closed() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "下载已取消").into());
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        archive.write_all(&buffer[..count])?;
    }
    Ok(())
}
