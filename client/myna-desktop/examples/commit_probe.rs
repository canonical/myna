//! Drive the real IBus injector from stdin, for compositor rigs.
//!
//! Lines: `acquire`, `commit BYTES`, `burst COUNT BYTES GAP_MS`,
//! `sleep MS`, `release`. Text is numbered six-byte words ("00001 "), so a
//! reader can tell which bytes arrived.

use std::io::BufRead;
use std::time::{Duration, Instant};

use myna_desktop::inject::ibus::IbusInjector;
use myna_desktop::{Injector, Target};

fn text(from: &mut usize, bytes: usize) -> String {
    let mut out = String::new();
    while out.len() < bytes {
        out.push_str(&format!("{:05} ", *from % 100_000));
        *from += 1;
    }
    out.truncate(bytes);
    out
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut injector = IbusInjector::connect().await.expect("connect to IBus");
    let mut target: Option<Box<dyn Target>> = None;
    let mut word = 0usize;
    let started = Instant::now();
    for line in std::io::stdin().lock().lines() {
        let line = line.expect("stdin");
        let args: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| args[i].parse::<usize>().expect("number");
        let at = started.elapsed().as_millis();
        match args.first().copied() {
            Some("acquire") => {
                target = Some(injector.acquire().await.expect("acquire"));
                println!("{at} acquired");
            }
            Some("commit") => {
                let t = text(&mut word, num(1));
                let r = target.as_mut().expect("acquired").commit(&t).await;
                println!("{at} commit {} {r:?}", t.len());
            }
            Some("burst") => {
                for _ in 0..num(1) {
                    let t = text(&mut word, num(2));
                    let r = target.as_mut().expect("acquired").commit(&t).await;
                    println!("{} commit {} {r:?}", started.elapsed().as_millis(), t.len());
                    tokio::time::sleep(Duration::from_millis(num(3) as u64)).await;
                }
            }
            Some("sleep") => tokio::time::sleep(Duration::from_millis(num(1) as u64)).await,
            Some("release") => {
                let r = target.take().expect("acquired").release().await;
                println!("{at} released {r:?}");
            }
            _ => println!("{at} ? {line}"),
        }
    }
}
