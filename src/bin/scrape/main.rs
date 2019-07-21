#![feature(async_await)]

use std::cell::RefCell;
use std::sync::mpsc::channel;

use threadpool::ThreadPool;
use tokio::runtime::current_thread::Runtime;

const NUMBER_OF_THREADS: usize = 32;
const LETTERS: &str = "ABCĆDEFGHIJKLŁMNOÓPQRSŚTUVWXYZŹŻ";

fn main() {
    use std::env;

    match env::args().nth(1) {
        Some(ref s) if s == "words" => scrape_words(),
        Some(ref s) if s == "defs" => scrape_defs(),
        _ => panic!("missing")
    }
}

fn get_ssid() -> String {
    Runtime::new()
        .unwrap()
        .block_on(dictionarium_vilnensis::get_ssid())
        .unwrap()
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

fn scrape_defs() {
    use backoff::Operation;
    use itertools::Itertools;
    use select::predicate::Attr;

    use std::fs::File;
    use std::io::{BufRead, BufReader};
    use std::io::Write;

    let ssid = get_ssid();
    let word_count = BufReader::new(File::open("words").unwrap()).lines().count();
    let pb = indicatif::ProgressBar::new(word_count as u64);
    pb.set_style(indicatif::ProgressStyle::default_bar().template("{msg} {wide_bar} {pos}/{len}"));

    let pool = ThreadPool::new(NUMBER_OF_THREADS);
    let (tx, rx) = channel();

    let mut output_file = File::create("output").unwrap();
    let word_file = File::open("words").unwrap();
    read_words_from(word_file)
        .map(|(id, word)| {
            let tx = tx.clone();
            let ssid = ssid.clone();
            pool.execute(move || {
                let mut backoff = backoff::ExponentialBackoff::default();
                let mut op = || {
                    let future = dictionarium_vilnensis::get_page(&ssid, 'A', 0, Some(id));
                    Runtime::new().unwrap().block_on(future)
                        .map_err(|e| e.into_backoff_error())
                };
                
                let document: select::document::Document = op
                    .retry(&mut backoff)
                    .expect("should have succeeded");

                let def = document.find(Attr("id", "haslo")).next().unwrap().inner_html();
                tx.send((word, def)).expect("should have sent");
            });
        })
        .chunks(NUMBER_OF_THREADS * 4)
        .into_iter()
        .flat_map(|chunk| rx.iter().take(chunk.count()))
        .inspect(|&(ref word, _)| {
            pb.inc(1);
            pb.set_message(word);
        })
        .for_each(|(word, def)| {
            writeln!(output_file, "{}\t{}", word, def).unwrap();
        });

    pb.finish();
}

fn scrape_counts() -> std::collections::HashMap<char, u64> {
    use backoff::Operation;
    use select::predicate::Attr;

    let ssid = get_ssid();
    let message = RefCell::new(String::from(LETTERS));
    let char_count = LETTERS.chars().count();

    let pb = indicatif::ProgressBar::new(char_count as u64);
    pb.set_message(&message.borrow());
    pb.set_style(indicatif::ProgressStyle::default_bar()
        .template("{msg} {wide_bar} {pos}/{len}"));

    let pool = ThreadPool::new(NUMBER_OF_THREADS);
    let (tx, rx) = channel();
    LETTERS
        .chars()
        .map(|letter| {
            let tx = tx.clone();
            let ssid = ssid.clone();
            pool.execute(move || {
                let mut backoff = backoff::ExponentialBackoff::default();
                let mut op = || {
                    let future = dictionarium_vilnensis::get_page(&ssid, letter, 0, None);
                    Runtime::new().unwrap().block_on(future)
                        .map_err(|e| e.into_backoff_error())
                };
                
                let document: select::document::Document = op
                    .retry(&mut backoff)
                    .expect("should have succeeded");
                    
                let re = regex::Regex::new(r"(\d+)-(\d+)/(\d+)").unwrap();
                let left = document.find(Attr("id", "listaHasel")).next().unwrap().children().nth(0).unwrap().text();
                let captures = re.captures(left.trim()).unwrap();
                let out_of: u64 = captures.get(3).unwrap().as_str().parse().unwrap();

                tx.send((letter, out_of)).expect("should have sent");
            });
        })
        .take(LETTERS.len())
        .map(|_| rx.recv().unwrap())
        .inspect(|(letter, _)| {
            pb.inc(1);
            let new_message = message.borrow_mut().replace(*letter, "-");
            *message.borrow_mut() = new_message;
            pb.set_message(&message.borrow());
        })
        .collect()
}

fn scrape_words() {
    use backoff::Operation;
    use select::predicate::{Attr, Child, Name};

    use std::fs::File;
    use std::io::Write;

    const PAGE_SIZE: u64 = 200;

    let ssid = get_ssid();
    let counts = scrape_counts();
    let total_count = counts.values().sum();

    let pb = indicatif::ProgressBar::new(total_count);
    pb.set_style(indicatif::ProgressStyle::default_bar().template("{eta} {wide_bar} {pos}/{len}"));

    let mut output_file = File::create("words").unwrap();
    let pool = ThreadPool::new(NUMBER_OF_THREADS);
    let (tx, rx) = channel();
    for (letter, count) in counts {
        for i in num::range_step(0, count, PAGE_SIZE) {
            let tx = tx.clone();
            let ssid = ssid.clone();
            let offset = i / PAGE_SIZE;
            pool.execute(move || {
                let mut backoff = backoff::ExponentialBackoff::default();
                let mut op = || {
                    let future = dictionarium_vilnensis::get_page(&ssid, letter, offset as u8, None);
                    Runtime::new().unwrap().block_on(future)
                        .map_err(|e| e.into_backoff_error())
                };
                
                let re = regex::Regex::new(r"javascript: haslo\((\d+)").unwrap();
                let document: select::document::Document = op
                    .retry(&mut backoff)
                    .expect("should have succeeded");

                let words = document.find(Child(Attr("id", "listaHasel"), Name("div"))).into_selection();
                for node in words.iter().take(words.len() - 1) {
                    let a = node.find(Name("a")).find(|c| c.attr("id").is_none()).unwrap();
                    let id: u32 = re.captures(a.attr("href").unwrap()).unwrap().get(1).unwrap().as_str().parse().unwrap();
                    let word = a.text();
                    tx.send((id, word)).expect("should have sent");
                }
            });
        }
    }	

    rx
        .into_iter()
        .take(total_count as usize)
        .inspect(|&(_, ref word)| {
            pb.inc(1);
            pb.set_message(&word);
        })
        .for_each(|(id, word)| {
            writeln!(&mut output_file, "{}\t{}", id, word).unwrap();
        });

    pb.finish();
}
