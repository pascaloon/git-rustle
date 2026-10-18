//! Compile with rustc -O --edition=2024; see docs/benchmarking.md.
//! Uses the production draft module without linking the desktop/GPU runtime.
#![allow(dead_code)]
use std::{hint::black_box, time::Instant};

// The draft reads only Plan.text. Git metadata, workers and GPUI are deliberately
// outside this component measurement; native opening measures their delivery.
mod git {
    pub mod operations {
        #[derive(Default)]
        pub struct Plan {
            pub text: String,
        }
    }
}
#[path = "../src/desktop/rebase.rs"]
mod rebase;
use rebase::{CommitAction, Draft};

fn main() {
    let plan = git::operations::Plan {
        text: (0..1000)
            .map(|i| format!("pick {i:040x} Commit {i} with a useful subject\n"))
            .collect(),
    };
    println!("[");
    for sample in -1..15 {
        let start = Instant::now();
        let mut draft = Draft::new(black_box(&plan));
        let load_ms = start.elapsed().as_secs_f64() * 1000.;
        black_box(&draft);

        let start = Instant::now();
        for step in 0..200 {
            black_box(&mut draft).set_action(1, CommitAction::ALL[step % 6]);
        }
        let action_us = start.elapsed().as_secs_f64() * 1_000_000. / 200.;

        let start = Instant::now();
        for step in 0..200 {
            black_box(&mut draft).move_entry(1, if step % 2 == 0 { 1 } else { -1 });
        }
        let move_us = start.elapsed().as_secs_f64() * 1_000_000. / 200.;

        draft.set_action(1, CommitAction::Reword);
        let start = Instant::now();
        for step in 0..200 {
            draft.entries[1].message = format!("Reworded commit {step}");
            black_box(&mut draft).refresh();
        }
        let message_us = start.elapsed().as_secs_f64() * 1_000_000. / 200.;

        let start = Instant::now();
        for _ in 0..200 {
            black_box(black_box(&draft).text());
        }
        let serialize_us = start.elapsed().as_secs_f64() * 1_000_000. / 200.;
        assert_eq!(draft.entries.len(), 1000);
        assert_eq!(draft.entries[1].key, 1);
        assert_eq!(draft.errors, 0);
        println!(
            "{{\"sample\":{sample},\"loadMs\":{load_ms},\"actionMeanUs\":{action_us},\"moveMeanUs\":{move_us},\"messageMeanUs\":{message_us},\"serializeMeanUs\":{serialize_us}}}{}",
            if sample == 14 { "" } else { "," }
        );
    }
    println!("]");
}
