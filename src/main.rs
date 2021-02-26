use base64::{decode, encode};
use clap::{load_yaml, App};
use futures::future::try_join_all;
use serde::{Deserialize, Deserializer};
use std::{collections::HashMap, fs::File, io::{Read, Write}, path::PathBuf, time::Duration, u64};

// const API_KEY: &str = "3953957dd37204f2622ff3361c5d6e87";
const API_KEY:&str = "0750521d6eee3603ac222d0422891eea";
const API_BASE_URL: &str = "http://api.convertio.co/convert";
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

#[derive(Deserialize)]
struct NewConversionResp {
    code: i32,
    error: Option<String>,
    data: Option<ConvertioData>,
}

#[derive(Deserialize)]
struct StatusConversionResp {
    code: i32,
    error: Option<String>,
    data: Option<ConvertioData>,
}

#[derive(Deserialize, Clone)]
struct ConvertioData {
    id: String,
    step: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_u64_or_empty_string")]
    step_percent: Option<u64>,
}

#[derive(Deserialize)]
struct FileData {
    content: String,
}

#[derive(Deserialize)]
struct FileDownloadResp {
    code: i32,
    data: FileData,
}

fn deserialize_u64_or_empty_string<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<u64> = Option::deserialize(deserializer).unwrap_or(Some(0));
    Ok(s)
}

struct ConversionTask {
    conversion_id: String,
    done: bool,
    input_file_name: String,
    output_format: String,
    progress: u64,
}

async fn start_conversion(
    input_file_name: &str,
    output_format: &str,
) -> Result<ConversionTask, Box<dyn std::error::Error>> {
    // Starts a new conversion
    let mut map = HashMap::new();
    map.insert("apikey", API_KEY);
    map.insert("input", "base64");

    let mut file = File::open(input_file_name).expect("file open failed");

    let mut buf = vec![];
    file.read_to_end(&mut buf).expect("file read failed");

    let encode_buf = encode(&buf);
    map.insert("file", encode_buf.as_str());
    map.insert("filename", input_file_name);
    map.insert("outputformat", output_format);

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}", API_BASE_URL).as_str())
        .json(&map)
        .send()
        .await?
        .json::<NewConversionResp>()
        .await?;

    if resp.code != 200 {
        return Err(format!("{}", resp.error.unwrap()).into());
    }

    let conversion_id = resp.data.unwrap().id;
    Ok(ConversionTask {
        conversion_id: conversion_id,
        done: false,
        input_file_name: input_file_name.to_owned(),
        output_format: output_format.to_owned(),
        progress: 0,
    })
}

async fn wait_for_status(task: &mut ConversionTask) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/{}/status", API_BASE_URL, task.conversion_id).as_str())
        .send()
        .await?
        .json::<StatusConversionResp>()
        .await?;
    if resp.code == 200 {
        if resp.data.clone().unwrap().step.as_deref().unwrap().clone() == "finish" {
            let client = reqwest::Client::new();
            let resp = client
                .get(format!("{}/{}/dl/base64", API_BASE_URL, task.conversion_id).as_str())
                .send()
                .await?
                .json::<FileDownloadResp>()
                .await?;
            if resp.code == 200 {
                let mut output_path = PathBuf::from(task.input_file_name.as_str());
                output_path.set_extension(task.output_format.as_str());
                let mut file = File::create(output_path).expect("create file failed");
                let decode_buf = decode(&resp.data.content).unwrap();
                file.write_all(&decode_buf).expect("write file failed");
            }
            task.done = true;
            task.progress = 100;
        } else {
            task.progress = resp
                .data
                .clone()
                .unwrap()
                .step_percent
                .as_ref()
                .unwrap()
                .clone();
        }
    }
    if resp.code != 200 {
        task.done = true;
        println!("{}", resp.error.unwrap())
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from(yaml).get_matches();

    let output_format = matches.value_of("format").unwrap();

    let input_file_names = matches.values_of("input").unwrap();

    let mut conversions: Vec<ConversionTask> = try_join_all(
        input_file_names.map(|input_file_name| start_conversion(input_file_name, output_format)),
    )
    .await
    .unwrap();

    // let m = MultiProgress::new();
    let sty = ProgressStyle::default_bar()
        .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}")
        .progress_chars("##-");

    let mut progress_bars = vec![];
    let m = MultiProgress::new();
    conversions.iter().for_each(|conversion| {
        let pb = m.add(ProgressBar::new(100));
        pb.set_style(sty.clone());
        pb.set_position(0);
        pb.set_message(&conversion.input_file_name);
        progress_bars.push(pb);
    });

    tokio::spawn(async move {
        m.join().unwrap();
    });

    loop {
        if conversions.len() == 0 {
            break;
        }
        try_join_all(
            conversions
                .iter_mut()
                .map(|conversion| wait_for_status(conversion)),
        )
        .await
        .unwrap();
        for (index, e) in conversions.iter().enumerate() {
            progress_bars[index].set_position(e.progress);
            progress_bars[index].set_message(&e.input_file_name);
            if e.progress==100 {
                progress_bars[index].finish_and_clear();
            }
        }
        conversions.retain(|conversion| !conversion.done);
        progress_bars.retain(|progress_bar|{!progress_bar.position()!=100});
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}
