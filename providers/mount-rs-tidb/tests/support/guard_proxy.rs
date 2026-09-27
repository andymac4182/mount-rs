//! Test-only unencrypted MySQL wire trace. Retains only counts and lengths.
use std::collections::BTreeMap;
use std::io;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub struct Insert {
    pub rows: usize,
    pub prepare_bytes: usize,
    pub execute_bytes: usize,
    pub volume_bytes: usize,
    pub node_bytes: usize,
}
#[derive(Default)]
pub struct Trace {
    pub armed: AtomicBool,
    pub inserts: Mutex<Vec<Insert>>,
    pub prepares: Mutex<usize>,
    pub drop_commit_ack: AtomicBool,
    pub commits: AtomicUsize,
    pub acks_dropped: AtomicUsize,
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
        let local = listener.local_addr().unwrap().port();
        url.set_host(Some("127.0.0.1")).unwrap();
        url.set_port(Some(local)).unwrap();
        let trace = Arc::new(Trace::default());
        let task = tokio::spawn({
            let trace = trace.clone();
            async move {
                let mut tasks = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        accepted=listener.accept()=> {
                            let (client,_)=accepted.unwrap();
                            let server=TcpStream::connect((host.as_str(),port)).await.unwrap();
                            client.set_nodelay(true).unwrap(); server.set_nodelay(true).unwrap();
                            let trace=trace.clone();
                            tasks.spawn(async move { relay(client,server,trace).await });
                        }
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
        self.trace.inserts.lock().unwrap().clear();
        *self.trace.prepares.lock().unwrap() = 0;
        self.trace.armed.store(true, Ordering::SeqCst);
    }
    pub fn end(&self) -> (Vec<Insert>, usize) {
        self.trace.armed.store(false, Ordering::SeqCst);
        (
            self.trace.inserts.lock().unwrap().clone(),
            *self.trace.prepares.lock().unwrap(),
        )
    }
}
async fn relay(client: TcpStream, server: TcpStream, trace: Arc<Trace>) {
    let (mut cr, mut cw) = client.into_split();
    let (mut sr, mut sw) = server.into_split();
    let pending = Arc::new(Mutex::new(None));
    let statements = Arc::new(Mutex::new(BTreeMap::<u32, (usize, usize)>::new()));
    let drop_response = Arc::new(AtomicBool::new(false));
    let response_drop = drop_response.clone();
    let response_trace = trace.clone();
    let pending_response = pending.clone();
    let statement_response = statements.clone();
    let responses = async move {
        while let Ok(p) = packet(&mut sr).await {
            if response_drop.swap(false, Ordering::SeqCst) {
                assert_eq!(
                    p[4], 0,
                    "lost-ACK injection requires a committed OK response"
                );
                response_trace.acks_dropped.fetch_add(1, Ordering::SeqCst);
                break;
            }
            if let Some(shape) = pending_response.lock().unwrap().take() {
                assert_eq!(p[4], 0, "guard prepare must succeed");
                let id = u32::from_le_bytes(p[5..9].try_into().unwrap());
                statement_response.lock().unwrap().insert(id, shape);
            }
            if cw.write_all(&p).await.is_err() {
                break;
            }
        }
    };
    let requests = async move {
        while let Ok(p) = packet(&mut cr).await {
            if p.len() > 5
                && p[4] == 0x03
                && p[5..].eq_ignore_ascii_case(b"COMMIT")
                && trace.armed.load(Ordering::SeqCst)
            {
                trace.commits.fetch_add(1, Ordering::SeqCst);
                if trace.drop_commit_ack.swap(false, Ordering::SeqCst) {
                    drop_response.store(true, Ordering::SeqCst);
                }
            }
            if p.len() > 5 && p[4] == 0x16 {
                let sql = &p[5..];
                if sql.starts_with(b"INSERT INTO mount_rs_tidb_inodes ") {
                    let rows = sql.iter().filter(|b| **b == b'?').count() / 4;
                    *pending.lock().unwrap() = Some((rows, p.len()));
                    if trace.armed.load(Ordering::SeqCst) {
                        *trace.prepares.lock().unwrap() += 1;
                    }
                }
            } else if p.len() >= 9 && p[4] == 0x17 {
                let id = u32::from_le_bytes(p[5..9].try_into().unwrap());
                if let Some(&(rows, prepare_bytes)) = statements.lock().unwrap().get(&id)
                    && trace.armed.load(Ordering::SeqCst)
                {
                    let (volume_bytes, node_bytes) = parameter_sizes(&p, rows);
                    trace.inserts.lock().unwrap().push(Insert {
                        rows,
                        prepare_bytes,
                        execute_bytes: p.len(),
                        volume_bytes,
                        node_bytes,
                    });
                }
            }
            if sw.write_all(&p).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {_=responses=>{},_=requests=>{}}
}

fn string_len(p: &[u8], offset: &mut usize) -> usize {
    let head = p[*offset];
    *offset += 1;
    match head {
        0..=250 => head as usize,
        252 => {
            let n = u16::from_le_bytes(p[*offset..*offset + 2].try_into().unwrap()) as usize;
            *offset += 2;
            n
        }
        253 => {
            let n = p[*offset] as usize
                | (p[*offset + 1] as usize) << 8
                | (p[*offset + 2] as usize) << 16;
            *offset += 3;
            n
        }
        254 => {
            let n = u64::from_le_bytes(p[*offset..*offset + 8].try_into().unwrap()) as usize;
            *offset += 8;
            n
        }
        _ => panic!("unexpected test string prefix"),
    }
}
fn parameter_sizes(p: &[u8], rows: usize) -> (usize, usize) {
    let count = rows * 4;
    let mut offset = 4 + 10 + count.div_ceil(8);
    assert_eq!(p[offset], 1);
    offset += 1 + count * 2;
    let mut volume = 0;
    let mut nodes = 0;
    for row in 0..rows {
        let n = string_len(p, &mut offset);
        if row == 0 {
            volume = n;
        } else {
            assert_eq!(volume, n);
        }
        offset += n + 16;
        let n = string_len(p, &mut offset);
        nodes += n;
        offset += n;
    }
    assert_eq!(offset, p.len());
    (volume, nodes)
}
