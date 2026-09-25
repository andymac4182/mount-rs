//! Test-only plaintext MySQL boundary controls. Retains SQL shapes/counts only.
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
};

#[derive(Default)]
pub struct Trace {
    pub armed: AtomicBool,
    pub queries: Mutex<Vec<String>>,
    pub guard_rows: AtomicUsize,
    pub commits: AtomicUsize,
    pub dropped_acks: AtomicUsize,
    pub drop_commit_ack: AtomicBool,
    pub abort_anchor_write: AtomicBool,
    pub pause_guards: AtomicBool,
    pub pause_anchor: AtomicBool,
    pub reached: Notify,
    pub resume: Notify,
}
pub struct Proxy {
    pub url: String,
    pub trace: Arc<Trace>,
    task: JoinHandle<()>,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn packet<R: AsyncReadExt + Unpin>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut h = [0; 4];
    r.read_exact(&mut h).await?;
    let n = h[0] as usize | (h[1] as usize) << 8 | (h[2] as usize) << 16;
    if n > 8 * 1024 * 1024 {
        return Err(io::Error::other("test packet bound"));
    }
    let mut p = vec![0; n + 4];
    p[..4].copy_from_slice(&h);
    r.read_exact(&mut p[4..]).await?;
    Ok(p)
}
impl Proxy {
    pub async fn new(url: &str) -> Self {
        let mut url = url::Url::parse(url).unwrap();
        assert_eq!(url.scheme(), "mysql");
        let host = url.host_str().unwrap().to_owned();
        let port = url.port().unwrap_or(3306);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        url.set_host(Some("127.0.0.1")).unwrap();
        url.set_port(Some(listener.local_addr().unwrap().port()))
            .unwrap();
        let trace = Arc::new(Trace::default());
        let task = tokio::spawn({
            let trace = trace.clone();
            async move {
                let mut tasks = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        accepted=listener.accept()=> { let (client,_)=accepted.unwrap(); let server=TcpStream::connect((host.as_str(),port)).await.unwrap(); client.set_nodelay(true).unwrap();server.set_nodelay(true).unwrap();let trace=trace.clone(); tasks.spawn(async move {relay(client,server,trace).await}); }
                        result=tasks.join_next(), if !tasks.is_empty()=> { result.unwrap().unwrap(); }
                    }
                }
            }
        });
        Self {
            url: url.to_string(),
            trace,
            task,
        }
    }
    pub fn begin(&self) {
        self.trace.queries.lock().unwrap().clear();
        self.trace.guard_rows.store(0, Ordering::SeqCst);
        self.trace.commits.store(0, Ordering::SeqCst);
        self.trace.armed.store(true, Ordering::SeqCst);
    }
    pub fn end(&self) -> (Vec<String>, usize) {
        self.trace.armed.store(false, Ordering::SeqCst);
        (
            self.trace.queries.lock().unwrap().clone(),
            self.trace.guard_rows.load(Ordering::SeqCst),
        )
    }
    pub async fn reached(&self) {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.trace.reached.notified(),
        )
        .await
        .expect("controlled SQL boundary reached");
    }
}
async fn relay(client: TcpStream, server: TcpStream, trace: Arc<Trace>) {
    let (mut cr, mut cw) = client.into_split();
    let (mut sr, mut sw) = server.into_split();
    let pending = Arc::new(Mutex::new(None::<String>));
    let statements = Arc::new(Mutex::new(BTreeMap::<u32, String>::new()));
    let guard_response = Arc::new(AtomicBool::new(false));
    let drop_response = Arc::new(AtomicBool::new(false));
    let responses = {
        let pending = pending.clone();
        let statements = statements.clone();
        let guard_response = guard_response.clone();
        let drop_response = drop_response.clone();
        let trace = trace.clone();
        async move {
            while let Ok(p) = packet(&mut sr).await {
                if drop_response.swap(false, Ordering::SeqCst) {
                    assert_eq!(
                        p[4], 0,
                        "postcommit injection requires actual server COMMIT OK"
                    );
                    trace.dropped_acks.fetch_add(1, Ordering::SeqCst);
                    break;
                }
                if let Some(sql) = pending.lock().unwrap().take() {
                    if p[4] == 0 {
                        let id = u32::from_le_bytes(p[5..9].try_into().unwrap());
                        statements.lock().unwrap().insert(id, sql);
                    }
                } else if guard_response.load(Ordering::SeqCst)
                    && p[4] == 0
                    && trace.armed.load(Ordering::SeqCst)
                {
                    // Prepared SELECT binary rows start with 0x00; column count,
                    // definitions and EOF use other markers. Only guard SELECTs arm it.
                    trace.guard_rows.fetch_add(1, Ordering::SeqCst);
                }
                if cw.write_all(&p).await.is_err() {
                    break;
                }
            }
        }
    };
    let requests = async move {
        while let Ok(p) = packet(&mut cr).await {
            let mut sql = None;
            if p.len() > 5 && p[4] == 0x16 {
                *pending.lock().unwrap() = Some(String::from_utf8(p[5..].to_vec()).unwrap());
                guard_response.store(false, Ordering::SeqCst);
            } else if p.len() >= 9 && p[4] == 0x17 {
                let id = u32::from_le_bytes(p[5..9].try_into().unwrap());
                sql = statements.lock().unwrap().get(&id).cloned();
            } else if p.len() > 5 && p[4] == 0x03 {
                sql = String::from_utf8(p[5..].to_vec()).ok();
            }
            if let Some(sql) = sql {
                let guard=sql.starts_with("SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards");
                guard_response.store(guard, Ordering::SeqCst);
                if trace.armed.load(Ordering::SeqCst) {
                    // All retained statements are controlled provider SQL, with
                    // prepared values kept entirely out of the trace.
                    trace.queries.lock().unwrap().push(sql.clone());
                    if (guard && trace.pause_guards.swap(false,Ordering::SeqCst)) || (sql.starts_with("SELECT revision,write_mode,backing_id,owner,fence,expires,namespace,delegation") && trace.pause_anchor.swap(false,Ordering::SeqCst)) {
                        trace.reached.notify_one();trace.resume.notified().await;
                    }
                    if sql.starts_with("UPDATE mount_rs_tidb_metadata SET revision=")
                        && trace.abort_anchor_write.swap(false, Ordering::SeqCst)
                    {
                        break;
                    }
                    if sql.eq_ignore_ascii_case("COMMIT") {
                        trace.commits.fetch_add(1, Ordering::SeqCst);
                        if trace.drop_commit_ack.swap(false, Ordering::SeqCst) {
                            drop_response.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
            if sw.write_all(&p).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {_=responses=>{},_=requests=>{}}
}
