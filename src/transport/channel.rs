use std::any::Any;
use std::io::{Read, Result, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytebuf_rs::bytebuf::ByteBuf;
use chashmap::CHashMap;
use mio::{Poll, PollOpt, Ready, Token};
use mio::net::TcpStream;
use rayon_core::ThreadPool;

use crate::core::eventloop::EventLoop;
use crate::handler::channel_handler_ctx_pipe::ChannelOutboundHandlerCtxPipe;

#[derive(Clone)]
pub enum ChannelOptions {
    NUMBER(usize),
    BOOL(bool),
}


pub struct Channel {
    id: Token,
    stream: TcpStream,
    closed: bool,
    eventloop: Arc<EventLoop>,
    ///
    /// 持有ChannelOutboundHandlerCtxPipe,用于写数据
    ///
    pub(crate) outbound_context_pipe: Option<Arc<Mutex<ChannelOutboundHandlerCtxPipe>>>,
}


impl Clone for Channel {
    fn clone(&self) -> Self {
        Channel {
            id: self.id.clone(),
            stream: self.stream.try_clone().unwrap(),
            closed: false,
            eventloop: self.eventloop.clone(),
            outbound_context_pipe: self.outbound_context_pipe.clone(),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        *self = source.clone()
    }
}

impl Channel {
    pub fn create(id: Token, eventloop: Arc<EventLoop>, stream: TcpStream,
    ) -> Channel {
        Channel {
            id,
            stream,
            closed: false,
            eventloop,
            outbound_context_pipe: None,
        }
    }

    pub(crate) fn remote_addr(&self) -> Result<SocketAddr> {
        self.stream.peer_addr()
    }

    pub(crate) fn local_addr(&self) -> Result<SocketAddr> {
        self.stream.local_addr()
    }

    pub(crate) fn write_bytebuf(&mut self, buf: &ByteBuf) {
        self.stream.write(buf.available_bytes());
        self.stream.flush();
    }

    ///
    /// 从pipeline 最开始写
    ///
    pub fn write_and_flush(&self, message: &dyn Any) {
        let pipe_arc = self.outbound_context_pipe.as_ref().unwrap();
        let pipe = pipe_arc.lock().unwrap();
        pipe.head_channel_write(message);
    }


    pub fn register(&self, poll: &Poll) {
        poll.register(
            &self.stream,
            self.id,
            Ready::readable(),
            PollOpt::edge(),
        );
    }

    pub fn read(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        self.stream.read_to_end(buf)
    }


    pub fn close(&mut self) {
        self.closed = true;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

///
/// 暴露channel 用
///
pub struct InboundChannelCtx {
    pub(crate) channel: Channel,
}

impl InboundChannelCtx {
    pub(crate) fn new(channel: Channel) -> InboundChannelCtx {
        InboundChannelCtx {
            channel
        }
    }

    pub(crate) fn write_and_flush(&mut self, message: &dyn Any) {
        self.channel.write_and_flush(message);
    }

    pub fn remote_addr(&self) -> Result<SocketAddr> {
        self.channel.remote_addr()
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.channel.local_addr()
    }


    pub fn is_active(&self) -> bool {
        self.channel.is_closed()
    }

    pub fn close(&mut self) {
        self.channel.close()
    }
}

pub struct OutboundChannelCtx {
    pub(crate) channel: Channel,
}

impl OutboundChannelCtx {
    pub(crate) fn new(channel: Channel) -> OutboundChannelCtx {
        OutboundChannelCtx {
            channel
        }
    }

    pub(crate) fn write_bytebuf(&mut self, buf: &ByteBuf) {
        self.channel.write_bytebuf(buf);
    }

    pub fn remote_addr(&self) -> Result<SocketAddr> {
        self.channel.remote_addr()
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.channel.local_addr()
    }
}