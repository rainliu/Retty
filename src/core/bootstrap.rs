use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::ops::Sub;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::Local;
use crossbeam::channel::{bounded, select};
use mio::net::TcpListener;
use mio::{Events, Poll, PollOpt, Ready, Token};

use crate::channel::channel_handler_ctx::{ChannelInboundHandlerCtx, ChannelOutboundHandlerCtx};
use crate::channel::channel_handler_ctx_pipe::{
    ChannelInboundHandlerCtxPipe, ChannelOutboundHandlerCtxPipe,
};
use crate::channel::handler::{HeadHandler, TailHandler};
use crate::channel::handler_pipe::{ChannelInboundHandlerPipe, ChannelOutboundHandlerPipe};
use crate::core::eventloop::{EventLoop, EventLoopGroup};
use crate::errors::RettyErrorKind;
use crate::transport::channel::{Channel, ChannelOptions};

struct Sessions {
    channel: Arc<Mutex<Channel>>,
    in_pipe: Arc<ChannelInboundHandlerCtxPipe>,
}

impl Sessions {
    fn new(ch: Arc<Mutex<Channel>>, pipe: Arc<ChannelInboundHandlerCtxPipe>) -> Sessions {
        Sessions {
            channel: ch,
            in_pipe: pipe,
        }
    }
}

pub struct Bootstrap {
    host: String,
    port: u16,
    boss_group: EventLoopGroup,
    worker_group: Option<Arc<EventLoopGroup>>,
    channel_inbound_handler_pipe_fn:
        Option<Arc<dyn Fn() -> ChannelInboundHandlerPipe + Send + Sync + 'static>>,
    channel_outbound_handler_pipe_fn:
        Option<Arc<dyn Fn() -> ChannelOutboundHandlerPipe + Send + Sync + 'static>>,
    opts: HashMap<String, ChannelOptions>,
    stopped: Arc<AtomicBool>,
    channel_container: Arc<Mutex<HashMap<Token, Arc<Mutex<Sessions>>>>>,
}

impl Bootstrap {
    pub fn new_server_bootstrap() -> Bootstrap {
        Bootstrap {
            host: "0.0.0.0".to_owned(),
            port: 1511,
            boss_group: EventLoopGroup::new(2),
            worker_group: None,
            channel_inbound_handler_pipe_fn: None,
            channel_outbound_handler_pipe_fn: None,
            opts: HashMap::new(),
            stopped: Arc::new(AtomicBool::new(false)),
            channel_container: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn initialize_inbound_handler_pipeline<F>(&mut self, pipe_fn: F) -> &mut Self
    where
        F: Fn() -> ChannelInboundHandlerPipe + Send + Sync + 'static,
    {
        self.channel_inbound_handler_pipe_fn = Some(Arc::new(Box::new(pipe_fn)));
        self
    }

    pub fn initialize_outbound_handler_pipeline<F>(&mut self, pipe_fn: F) -> &mut Self
    where
        F: Fn() -> ChannelOutboundHandlerPipe + Send + Sync + 'static,
    {
        self.channel_outbound_handler_pipe_fn = Some(Arc::new(Box::new(pipe_fn)));
        self
    }

    // 设置 worker_group
    pub fn worker_group(&mut self, n: usize) -> &mut Self {
        self.worker_group = Some(Arc::new(EventLoopGroup::new(n)));
        self
    }

    /// set ttl in ms
    pub fn opt_ttl_ms(&mut self, ttl: usize) -> &mut Self {
        self.opts
            .insert("ttl".to_owned(), ChannelOptions::NUMBER(ttl));
        self
    }

    /// set linger in ms
    pub fn opt_linger_ms(&mut self, linger: usize) -> &mut Self {
        self.opts
            .insert("linger".to_owned(), ChannelOptions::NUMBER(linger));
        self
    }

    /// set tcp nodelay
    pub fn opt_nodelay(&mut self, nodelay: bool) -> &mut Self {
        self.opts
            .insert("nodelay".to_owned(), ChannelOptions::BOOL(nodelay));
        self
    }

    pub fn opt_keep_alive_ms(&mut self, keep_alive: usize) -> &mut Self {
        self.opts
            .insert("keep_alive".to_owned(), ChannelOptions::NUMBER(keep_alive));
        self
    }

    pub fn opt_recv_buf_size(&mut self, buf_size: usize) -> &mut Self {
        self.opts
            .insert("recv_buf_size".to_owned(), ChannelOptions::NUMBER(buf_size));
        self
    }

    pub fn opt_send_buf_size(&mut self, buf_size: usize) -> &mut Self {
        self.opts
            .insert("send_buf_size".to_owned(), ChannelOptions::NUMBER(buf_size));
        self
    }

    pub fn opt_read_idle_timeout_ms(&mut self, ms: usize) -> &mut Self {
        self.opts.insert(
            "read_idle_timeout_ms".to_owned(),
            ChannelOptions::NUMBER(ms),
        );
        self
    }

    /// bind address and port
    pub fn bind(&mut self, host: &str, port: u16) -> &mut Self {
        self.host = host.to_owned();
        self.port = port;
        self
    }

    ///
    pub fn terminate(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(ref group) = &self.worker_group {
            group.event_loop_group().iter().for_each(|g| {
                g.shutdown();
            });
        }
    }

    pub fn start(&mut self) {
        let boss_group = &mut self.boss_group;
        let boss_eventloop = boss_group.next().unwrap();
        let idle_task_event_loop = boss_group.next().unwrap();
        let work_group = match &self.worker_group {
            None => panic!("work_group error"),
            Some(g) => Arc::clone(g),
        };
        let ip_addr = self.host.parse().unwrap();
        let sock_addr = Arc::new(SocketAddr::new(ip_addr, self.port));

        let opts = self.opts.clone();
        let stopped = Arc::clone(&self.stopped);

        let channel_inbound_handler_pipe_fn =
            &self.channel_inbound_handler_pipe_fn.as_ref().unwrap();
        let channel_inbound_handler_pipe_fn = Arc::clone(channel_inbound_handler_pipe_fn);
        let channel_outbound_handler_pipe_fn =
            &self.channel_outbound_handler_pipe_fn.as_ref().unwrap();
        let channel_outbound_handler_pipe_fn = Arc::clone(channel_outbound_handler_pipe_fn);

        let channel_container = Arc::clone(&self.channel_container);
        idle_task_event_loop.excutor.spawn(move || {
            let (s, r) = bounded::<Token>(1024);
            loop {
                for (k, sess) in channel_container.lock().unwrap().iter() {
                    let sess = sess.lock().unwrap();
                    let channel = sess.channel.lock().unwrap();
                    if (Local::now().timestamp_millis() as u64).sub(channel.last_read_time_ms()) > channel.read_idle_timeout_ms() {
                        let _ = s.send(*k);
                    }
                }
                select! {
                    recv(r)->key=>{
                        let mut channel_container = channel_container.lock().unwrap();
                        let sess_opt   =  channel_container.remove( &key.unwrap());
                        match sess_opt{
                            Some(session) =>{
                                     let  session = session.lock().unwrap();
                                      let pipe =   &session.in_pipe;
                                     let read_timeout_err = RettyErrorKind::new(ErrorKind::TimedOut ,  "ReadIdleTimeout".to_string());
                                      pipe.head_channel_exception(read_timeout_err);
                            },
                            None =>{}
                        }
                    },
                    default(Duration::from_millis(5000)) =>{}
                }
                thread::sleep(Duration::from_millis(1000))
            }
        });

        let channel_container = Arc::clone(&self.channel_container);
        boss_eventloop.excutor.spawn(move || {
            let mut events = Events::with_capacity(1024);
            let mut ch_id: usize = 1;

            let listener = match TcpListener::bind(&sock_addr) {
                Ok(s) => {
                    println!("[High performance I/O framework written by Rust inspired by Netty]");
                    println!(
                        "[Retty server is listening : {:?} : {:?}]",
                        sock_addr.ip(),
                        sock_addr.port()
                    );
                    s
                }
                Err(e) => {
                    println!("error : {:?}", e);
                    panic!("server is not started:{:?}", e)
                }
            };

            let sel = Poll::new().unwrap();
            // 将监听器绑定在selector上 , 打上Token(0)的标记，注册read事件, 也就是只监听Tcplistener的事件，后面是监听TcpStream的事件
            sel.register(&listener, Token(0), Ready::readable(), PollOpt::edge())
                .unwrap();
            // 循环event_loop,启动reactor线程
            work_group.event_loop_group().iter().for_each(|e| e.run());
            //当服务器没有停的时候
            while !stopped.load(Ordering::Relaxed) {
                let event_loop = work_group.event_loop_group()
                    [ch_id % work_group.event_loop_group().len()]
                .clone();
                // 取出selector中的事件集合
                match sel.poll(&mut events, Some(Duration::from_millis(200))) {
                    Ok(_) => {}
                    Err(_) => {
                        continue;
                    }
                }
                // 循环事件，监听accept
                for _e in events.iter() {
                    let (sock, _) = match listener.accept() {
                        Ok((s, a)) => (s, a),
                        Err(_) => {
                            continue;
                        }
                    };

                    let channel = Channel::create(
                        Token(ch_id),
                        opts.clone(),
                        event_loop.clone(),
                        sock.try_clone().unwrap(),
                    );

                    let channel = Arc::new(Mutex::new(channel));
                    let outbound_ctx_pipe = Bootstrap::create_channel_outbound_ctx_pipe(
                        channel_outbound_handler_pipe_fn.clone(),
                        event_loop.clone(),
                        channel.clone(),
                    );
                    let inbound_ctx_pipe = Bootstrap::create_channel_inbound_ctx_pipe(
                        channel_inbound_handler_pipe_fn.clone(),
                        event_loop.clone(),
                        channel.clone(),
                        Arc::new(Mutex::new(outbound_ctx_pipe)),
                    );
                    event_loop
                        .clone()
                        .attach(ch_id, channel.clone(), inbound_ctx_pipe.clone());
                    let sessions = Arc::new(Mutex::new(Sessions::new(
                        channel.clone(),
                        Arc::new(inbound_ctx_pipe.clone()),
                    )));
                    channel_container
                        .lock()
                        .unwrap()
                        .insert(Token(ch_id), sessions.clone());
                    ch_id = Bootstrap::incr_id(ch_id);
                }
            }
        });
    }

    #[inline]
    fn incr_id(cur_id: usize) -> usize {
        if cur_id == usize::MAX {
            0
        } else {
            cur_id + 1
        }
    }

    ///
    /// 创建入站处理pipeline
    ///
    fn create_channel_inbound_ctx_pipe(
        in_channel_handler_pipe_fn: Arc<dyn Fn() -> ChannelInboundHandlerPipe + Send + Sync>,
        event_loop: Arc<EventLoop>,
        channel: Arc<Mutex<Channel>>,
        out_pipe: Arc<Mutex<ChannelOutboundHandlerCtxPipe>>,
    ) -> ChannelInboundHandlerCtxPipe {
        // 创建ChannelHandlerPipe , 每一个连接创建自己的一套pipeline
        let mut channel_handler_pipe: ChannelInboundHandlerPipe = (in_channel_handler_pipe_fn)();
        //添加头handler
        channel_handler_pipe.add_first(Box::new(HeadHandler::new()));
        // 创建ChannelHandlerCtxPipe
        let mut channel_handler_context_pipe = ChannelInboundHandlerCtxPipe::new();
        for _i in 0..channel_handler_pipe.handlers.len() {
            let handler = channel_handler_pipe.handlers.remove(0);
            let id = handler.id().clone();
            let handler_arc = Arc::new(Mutex::new(handler));
            let ctx = Arc::new(Mutex::new(ChannelInboundHandlerCtx::new(
                id,
                event_loop.clone(),
                channel.clone(),
                handler_arc.clone(),
                Some(out_pipe.clone()),
            )));
            channel_handler_context_pipe.add_last(ctx, handler_arc.clone());
        }

        let mut enumerate = channel_handler_context_pipe
            .channel_handler_ctx_pipe
            .iter()
            .enumerate();
        let ctx_pipe_len = channel_handler_context_pipe.channel_handler_ctx_pipe.len();

        let head_ctx = channel_handler_context_pipe
            .channel_handler_ctx_pipe
            .get(0)
            .unwrap()
            .clone();
        let head_handler = head_ctx.lock().unwrap().handler.clone();

        for _i in 0..ctx_pipe_len {
            let (_j, ctx) = enumerate.next().unwrap();

            let mut curr = ctx.lock().unwrap();
            curr.channel_handler_ctx_pipe = Some(channel_handler_context_pipe.clone());
            if _j == 0 {
                curr.head_handler = None;
                curr.head_ctx = None;
            } else {
                curr.head_handler = Some(head_handler.clone());
                curr.head_ctx = Some(head_ctx.clone());
            }
            let next_context = channel_handler_context_pipe
                .channel_handler_ctx_pipe
                .get(_j + 1);
            match next_context {
                Some(next_ctx) => {
                    let next_ctx_clone = next_ctx.clone();
                    let next_ctx_guard = next_ctx.lock().unwrap();
                    curr.next_ctx = Some(next_ctx_clone);
                    curr.next_handler = Some(next_ctx_guard.handler.clone());
                }
                None => {
                    curr.next_ctx = None;
                    curr.next_handler = None;
                }
            }
        }
        channel_handler_context_pipe.clone()
    }

    ///
    /// 创建出站处理器pipeline
    ///
    fn create_channel_outbound_ctx_pipe(
        out_channel_handler_pipe_fn: Arc<dyn Fn() -> ChannelOutboundHandlerPipe + Send + Sync>,
        event_loop: Arc<EventLoop>,
        channel: Arc<Mutex<Channel>>,
    ) -> ChannelOutboundHandlerCtxPipe {
        // 创建ChannelHandlerPipe , 每一个连接创建自己的一套pipeline
        let mut channel_handler_pipe: ChannelOutboundHandlerPipe = (out_channel_handler_pipe_fn)();
        // 创建ChannelHandlerCtxPipe
        let mut channel_handler_context_pipe = ChannelOutboundHandlerCtxPipe::new();
        //将handler pipeline 反序
        channel_handler_pipe.handlers.reverse();
        //
        // 添加TailHandler，追加到最后面
        //
        channel_handler_pipe.add_last(Box::new(TailHandler::new()));

        for _i in 0..channel_handler_pipe.handlers.len() {
            let handler = channel_handler_pipe.handlers.remove(0);
            let id = handler.id().clone();
            let handler_arc = Arc::new(Mutex::new(handler));
            let ctx = Arc::new(Mutex::new(ChannelOutboundHandlerCtx::new(
                id,
                event_loop.clone(),
                channel.clone(),
                handler_arc.clone(),
            )));
            channel_handler_context_pipe.add_last(ctx, handler_arc.clone());
        }

        let mut enumerate = channel_handler_context_pipe
            .channel_handler_ctx_pipe
            .iter()
            .enumerate();
        let ctx_pipe_len = channel_handler_context_pipe.channel_handler_ctx_pipe.len();

        let head_ctx = channel_handler_context_pipe
            .channel_handler_ctx_pipe
            .get(0)
            .unwrap()
            .clone();
        let head_handler = head_ctx.lock().unwrap().handler.clone();

        for _i in 0..ctx_pipe_len {
            let (_j, ctx) = enumerate.next().unwrap();
            let mut curr = ctx.lock().unwrap();
            curr.channel_handler_ctx_pipe = Some(channel_handler_context_pipe.clone());
            if _j == 0 {
                curr.head_handler = None;
                curr.head_ctx = None;
            } else {
                curr.head_handler = Some(head_handler.clone());
                curr.head_ctx = Some(head_ctx.clone());
            }
            let next_context = channel_handler_context_pipe
                .channel_handler_ctx_pipe
                .get(_j + 1);
            match next_context {
                Some(next_ctx) => {
                    let next_ctx_clone = next_ctx.clone();
                    let next_ctx_guard = next_ctx.lock().unwrap();
                    curr.next_ctx = Some(next_ctx_clone);
                    curr.next_handler = Some(next_ctx_guard.handler.clone());
                }
                None => {
                    curr.next_ctx = None;
                    curr.next_handler = None;
                }
            }
        }
        channel_handler_context_pipe.clone()
    }
}
