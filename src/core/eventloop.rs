use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use bytebuf_rs::bytebuf::ByteBuf;
use chashmap::CHashMap;
use mio::{Events, Poll, Token};
use rayon_core::ThreadPool;

use crate::channel::channel_handler_ctx_pipe::ChannelInboundHandlerCtxPipe;
use crate::errors::RettyErrorKind;
use crate::transport::channel::Channel;

pub struct EventLoop {
    pub(crate) excutor: Arc<ThreadPool>,
    pub(crate) selector: Arc<Poll>,
    pub(crate) channel_map: Arc<CHashMap<Token, Arc<Mutex<Channel>>>>,
    pub(crate) channel_inbound_handler_ctx_pipe_map:
        Arc<CHashMap<Token, ChannelInboundHandlerCtxPipe>>,
    pub(crate) stopped: Arc<AtomicBool>,
}

impl EventLoop {
    pub fn new(i: usize) -> EventLoop {
        EventLoop {
            excutor: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .thread_name(move |_| format!("eventloop-{}", i))
                    .build()
                    .unwrap(),
            ),
            selector: Arc::new(Poll::new().unwrap()),
            channel_map: Arc::new(CHashMap::new()),
            channel_inbound_handler_ctx_pipe_map: Arc::new(CHashMap::new()),
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    pub(crate) fn attach(
        &self,
        id: usize,
        ch: Arc<Mutex<Channel>>,
        ctx_inbound_ctx_pipe: ChannelInboundHandlerCtxPipe,
    ) {
        let channel = ch.clone();
        let channel_2 = ch;
        // 一个channel注册一个selector
        {
            let channel = channel.lock().unwrap();
            channel.register(&self.selector);
        }
        {
            ctx_inbound_ctx_pipe.head_channel_active();
        }
        self.channel_inbound_handler_ctx_pipe_map
            .insert_new(Token(id), ctx_inbound_ctx_pipe);
        self.channel_map.insert_new(Token(id), channel_2);
    }

    pub(crate) fn run(&self) {
        let selector = Arc::clone(&self.selector);
        let channel_map = Arc::clone(&self.channel_map);
        let channel_inbound_ctx_pipe_map = Arc::clone(&self.channel_inbound_handler_ctx_pipe_map);
        let stopped = Arc::clone(&self.stopped);

        self.excutor.spawn(move || {
            let mut events = Events::with_capacity(1024);
            while !stopped.load(Ordering::Relaxed) {
                selector
                    .poll(&mut events, Some(Duration::from_millis(200)))
                    .unwrap();

                for e in events.iter() {
                    let channel = match channel_map.remove(&e.token()) {
                        Some(ch) => {
                            let mut buf: Vec<u8> = Vec::with_capacity(65535);
                            let ch_clone = ch.clone();
                            let mut ch = ch.lock().unwrap();
                            let ch_ret = match ch.read(&mut buf) {
                                Ok(0) => {
                                    ch.close();
                                    None
                                }
                                Ok(_) => None,
                                Err(e) if e.kind() == ErrorKind::WouldBlock => None,
                                Err(e) => Some(e),
                            };
                            if !ch.is_closed() {
                                channel_map.insert_new(e.token(), ch_clone);
                            }
                            Some((ch.clone(), buf.clone(), ch_ret))
                        }
                        None => None,
                    };
                    if let Some((ch, buf, err)) = channel {
                        if ch.is_closed() {
                            {
                                let ctx_pipe =
                                    channel_inbound_ctx_pipe_map.get_mut(&e.token()).unwrap();
                                ctx_pipe.head_channel_inactive();
                            }
                        }
                        if !ch.is_closed() {
                            if let Some(err) = err {
                                let ctx_pipe =
                                    channel_inbound_ctx_pipe_map.get_mut(&e.token()).unwrap();
                                let error: RettyErrorKind = err.into();
                                ctx_pipe.head_channel_exception(error);
                            } else {
                                let mut bytebuf = ByteBuf::new_from(&buf[..]);
                                let ctx_pipe =
                                    channel_inbound_ctx_pipe_map.get_mut(&e.token()).unwrap();
                                ctx_pipe.head_channel_read(&mut bytebuf);
                            }
                        }
                    }
                }
            }
        });
    }

    pub fn schedule_delayed<F>(&self, task: F, delay_ms: usize)
    where
        F: FnOnce() + Send + 'static,
    {
        thread::sleep(Duration::from_millis(delay_ms as u64));
        self.excutor.spawn(task)
    }
}

#[derive(Clone)]
pub struct EventLoopGroup {
    group: Vec<Arc<EventLoop>>,
    evenetloop_num: usize,
    next: usize,
}

impl EventLoopGroup {
    pub fn new(n: usize) -> EventLoopGroup {
        let mut _group = Vec::<Arc<EventLoop>>::new();
        for _i in 0..n {
            _group.push(Arc::new(EventLoop::new(_i)));
        }
        EventLoopGroup {
            group: _group,
            evenetloop_num: n,
            next: 0,
        }
    }

    pub fn new_default_event_loop_group(n: usize) -> EventLoopGroup {
        EventLoopGroup::new(n)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Arc<EventLoop>> {
        if self.next > self.evenetloop_num {
            self.next = 0;
        }
        self.next += 1;
        Some(self.group.get(self.next - 1).unwrap().clone())
    }

    pub fn execute<F>(&mut self, task: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let executor = self.next().unwrap();
        executor.excutor.spawn(task);
    }

    pub fn event_loop_group(&self) -> &Vec<Arc<EventLoop>> {
        &self.group
    }
}
