#![feature(async_await, async_closure, type_ascription)]

use std::cell::RefCell;

use dictionarium_vilnensis as dv;

use tokio::runtime::current_thread::Runtime;

fn main() {
    use std::env;

    better_panic::install();

    match env::args().nth(1) {
        Some(ref s) if s == "words" => {
            Runtime::new().unwrap().block_on(scrape_words());
        }
        Some(ref s) if s == "defs" => {
            Runtime::new().unwrap().block_on(scrape_defs());
        }
        _ => panic!("missing")
    }
}

fn read_words_from<R>(input: R) -> impl Iterator<Item = (u32, String)>
    where R: std::io::Read {
    use itertools::Itertools;
    use std::io::BufRead;

    std::io::BufReader::new(input)
        .lines()
        .map(|line| line.unwrap().trim().to_string())
        .map(|line| line.split('\t').map(String::from).tuples().next().unwrap())
        .map(|(id, word)| (id.parse().unwrap(), word))
}

async fn scrape_defs() {
    use futures::future::FutureExt;
    use futures::stream::StreamExt;
    
    use std::fs::File;
    use std::io::{BufRead, BufReader};
    use std::io::Write;

    const BUFFER_SIZE: usize = 64;

    let ssid = dv::get_ssid().await.expect("should have gotten the SSID");

    let word_count = BufReader::new(File::open("words").unwrap()).lines().count();

    let pb = indicatif::ProgressBar::new(word_count as u64);
    pb.set_style(indicatif::ProgressStyle::default_bar().template("{msg} {wide_bar} {pos}/{len}"));

    let mut output_file = File::create("defs").unwrap();
    let word_file = File::open("words").unwrap();
    let fut = futures::stream::iter(read_words_from(word_file)
        .map(|(id, word)| {
            let ssid = ssid.clone();
            dv::get_def_with_backoff(ssid, id).map(move |def| (id, word, def))
        }))
        .buffered(BUFFER_SIZE)
        .inspect(|&(_, ref word, _)| {
            pb.inc(1);
            pb.set_message(word);
        })
        .for_each(|(id, word, def)| {
            writeln!(output_file, "{}\t{}\t{}", id, word, def).unwrap();
            futures::future::ready(())
        });

    fut.await;

    pb.finish();
}

async fn scrape_counts(ssid: String) -> std::collections::HashMap<char, u64> {
    use futures::stream::StreamExt;

    const LETTERS: &str = "ABCĆDEFGHIJKLŁMNOÓPQRSŚTUVWXYZŹŻ";

    let message = RefCell::new(String::from(LETTERS));
    let char_count = LETTERS.chars().count();

    let pb = indicatif::ProgressBar::new(char_count as u64);
    pb.set_message(&message.borrow());
    pb.set_style(indicatif::ProgressStyle::default_bar()
        .template("{msg} {wide_bar} {pos}/{len}"));

    LETTERS
        .chars()
        .map(|letter| {
            let ssid = ssid.clone();
            dv::scrape_letter_count_with_backoff(ssid, letter)
        })
        .collect::<futures::stream::FuturesUnordered<_>>()
        .inspect(|(letter, _)| {
            pb.inc(1);
            let new_message = message.borrow_mut().replace(*letter, "-");
            *message.borrow_mut() = new_message;
            pb.set_message(&message.borrow());
        })
        .collect::<std::collections::HashMap<_, _>>()
        .await
}

async fn get_words(ssid: String, counts: std::collections::HashMap<char, u64>)
    -> impl futures::Stream<Item = (u32, String)> {
    use futures::stream::StreamExt;

    const BUFFER_SIZE: usize = 16;
    const PAGE_SIZE: u64 = 200;

    futures::stream::iter(counts.into_iter()
        .flat_map(move |(letter, count)| {
            let ssid = ssid.clone();
            num::range_step(0, count, PAGE_SIZE).map(move |i| {
                let ssid = ssid.clone();
                dv::get_words_from_page_with_backoff(ssid, letter, (i / PAGE_SIZE) as u16)
            })
        }))
        .buffered(BUFFER_SIZE)
        .map(Vec::into_iter)
        .map(futures::stream::iter)
        .flatten()
}

async fn scrape_words() {
    use futures::future::FutureExt;
    use futures::stream::StreamExt;

    use std::fs::File;
    use std::io::Write;
    
    let ssid = dv::get_ssid().await.expect("should have gotten the SSID");
    let counts = scrape_counts(ssid.clone()).await;
    let total_count = counts.values().sum();

    let pb = indicatif::ProgressBar::new(total_count);
    pb.set_style(indicatif::ProgressStyle::default_bar().template("{eta} {wide_bar} {pos}/{len}"));

    let mut output_file = File::create("words").unwrap();
    let fut = get_words(ssid, counts)
        .flatten_stream()
        .for_each(|(id, ref word)| {
            pb.inc(1);
            pb.set_message(&word);
            writeln!(&mut output_file, "{}\t{}", id, word).unwrap();
            futures::future::ready(())
        });

    fut.await;

    pb.finish();
}
