//! Loopback-only fixture for opt-in native and engine remote-file regressions.
use crate::remote::{Provider, Remote};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};
pub(crate) struct Api {
    pub remote: Remote,
    pub blocked: Arc<AtomicBool>,
    pub release: Arc<AtomicBool>,
    pub requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
pub(crate) fn id(n: u8) -> gix::ObjectId {
    gix::ObjectId::from_bytes_or_panic(&[n; 20])
}
impl Api {
    pub fn start(provider: Provider) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut remote = Remote::parse(
            "origin",
            "https://ghost-files-fixture.invalid/team/repo",
            Some(provider.key()),
        )
        .unwrap();
        remote.endpoint = format!(
            "http://{}/repository/branches",
            listener.local_addr().unwrap()
        );
        let blocked = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let (b, r, s, q) = (
            blocked.clone(),
            release.clone(),
            stop.clone(),
            requests.clone(),
        );
        let worker = thread::spawn(move || {
            let mut clients = Vec::new();
            while !s.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                assert!(q.fetch_add(1, Ordering::Relaxed) < 128);
                let (b, r, s) = (b.clone(), r.clone(), s.clone());
                clients.push(thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut reader=BufReader::new(stream.try_clone().unwrap());
                    let mut request=String::new();reader.read_line(&mut request).unwrap();
                    for _ in 0..100 {let mut line=String::new();if reader.read_line(&mut line).unwrap()==0 || line=="\r\n" {break}}
                    let tree=request.contains("/git/trees/") || request.contains("/tree?");
                    let pair=request.contains("/compare?");
                    if (tree || pair) && request.contains(&id(1).to_string()) && !r.load(Ordering::Acquire) {
                        b.store(true,Ordering::Release);
                        while !r.load(Ordering::Acquire) && !s.load(Ordering::Acquire){thread::sleep(Duration::from_millis(1));}
                    }
                    let failed=request.contains(&id(4).to_string());
                    let body=if pair {
                        serde_json::json!({"compare_timeout":false,"diffs":[{"new_path":"remote-example.txt","old_path":"remote-example.txt","a_mode":"100644","b_mode":"100644","diff":"@@ -1 +1 @@\n-old\n+remote contents\n"}]})
                    } else if tree {
                        let blob=if request.contains(&id(2).to_string()) {id(8)}else{id(9)};
                        let entry=serde_json::json!({"path":"remote-example.txt","sha":blob.to_string(),"id":blob.to_string(),"mode":"100644","type":"blob"});
                        if provider==Provider::Github {serde_json::json!({"tree":[entry],"truncated":false})}else{serde_json::json!([entry])}
                    } else if request.contains("/commits/") {
                        if provider==Provider::Github {serde_json::json!({"files":[{"filename":"remote-example.txt","status":"added","sha":id(8).to_string(),"additions":1,"deletions":0}]})}
                        else {serde_json::json!([{"new_path":"remote-example.txt","old_path":"remote-example.txt","new_file":true,"diff":"@@ -0,0 +1 @@\n+remote contents\n"}])}
                    } else if request.contains("/git/blobs/") || request.contains("/files/") {serde_json::Value::Null}
                    else {panic!("Unexpected loopback fixture request: {request}")};
                    let body=if body.is_null(){b"remote contents\n".to_vec()}else{serde_json::to_vec(&body).unwrap()};
                    let code=if failed {"503 Unavailable"}else{"200 OK"};
                    let _=write!(stream,"HTTP/1.1 {code}\r\nContent-Length: {}\r\nX-Next-Page: \r\nConnection: close\r\n\r\n",body.len());
                    let _=stream.write_all(&body);
                }));
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        Self {
            remote,
            blocked,
            release,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    pub fn unblock(&self) {
        self.release.store(true, Ordering::Release);
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.unblock();
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

/// Build remote rows without fetching any objects or modifying refs.
pub(crate) fn install(app: &mut crate::app::App, remote: &Remote) {
    let head = app.head_id().unwrap();
    let original = app.history.clone();
    let template = app
        .details
        .as_ref()
        .filter(|d| d.id == head)
        .unwrap()
        .clone();
    let mut history = crate::model::History::default();
    for n in (1..=4).rev() {
        let mut details = template.clone();
        details.id = id(n);
        details.parents = vec![head];
        details.commit_time += i64::from(n);
        details.message = format!("Remote fixture commit {n}");
        let mut row = original.commits[original.index_of(&head).unwrap()];
        row.id = details.id;
        row.parents = history.push_parents(&[head]);
        row.commit_time = details.commit_time;
        row.author = history.push_text(details.author_name.as_bytes());
        row.subject = history.push_text(details.message.as_bytes());
        history.commits.push(row);
        app.ghost_sources.insert(details.id, remote.clone());
        app.ghosts.insert(details.id, details);
    }
    history.append(original);
    history.complete = true;
    let mut layout = crate::graph::layout::Layout::new();
    history.cells.clear();
    history.max_lanes = 0;
    for index in 0..history.len() {
        let row = layout.push(&history.commits[index].id, history.parents(index));
        let cells = history.push_cells(&row.cells);
        history.commits[index].row = cells;
        history.commits[index].node_lane = row.node_lane;
    }
    app.history = history;
    app.needs_redraw = true;
}
