use bytebuf_rs::bytebuf::ByteBuf;
use std::any::Any;

use crate::channel::channel_handler_ctx::{ChannelInboundHandlerCtx, ChannelOutboundHandlerCtx};
use crate::errors::RettyErrorKind;

pub trait ChannelInboundHandler {
    fn id(&self) -> String;
    fn channel_active(&mut self, channel_handler_ctx: &mut ChannelInboundHandlerCtx);
    fn channel_inactive(&mut self, channel_handler_ctx: &mut ChannelInboundHandlerCtx);
    fn channel_read(
        &mut self,
        channel_handler_ctx: &mut ChannelInboundHandlerCtx,
        message: &mut dyn Any,
    );
    fn channel_exception(
        &mut self,
        channel_handler_ctx: &mut ChannelInboundHandlerCtx,
        error: RettyErrorKind,
    );
}

pub trait ChannelOutboundHandler {
    fn id(&self) -> String;
    fn channel_write(
        &mut self,
        channel_handler_ctx: &mut ChannelOutboundHandlerCtx,
        message: &mut dyn Any,
    );
}

pub(crate) struct HeadHandler {}

impl ChannelInboundHandler for HeadHandler {
    fn id(&self) -> String {
        String::from("HEAD")
    }

    fn channel_active(&mut self, channel_handler_ctx: &mut ChannelInboundHandlerCtx) {
        channel_handler_ctx
            .channel()
            .set_last_read_time(chrono::Local::now().timestamp_millis() as u64);
        channel_handler_ctx.fire_channel_active();
    }

    fn channel_inactive(&mut self, channel_handler_ctx: &mut ChannelInboundHandlerCtx) {
        channel_handler_ctx.fire_channel_inactive();
    }

    fn channel_read(
        &mut self,
        channel_handler_ctx: &mut ChannelInboundHandlerCtx,
        message: &mut dyn Any,
    ) {
        channel_handler_ctx
            .channel()
            .set_last_read_time(chrono::Local::now().timestamp_millis() as u64);
        channel_handler_ctx.fire_channel_read(message);
    }

    fn channel_exception(
        &mut self,
        channel_handler_ctx: &mut ChannelInboundHandlerCtx,
        error: RettyErrorKind,
    ) {
        channel_handler_ctx.fire_channel_exception(error);
    }
}

impl HeadHandler {
    pub(crate) fn new() -> HeadHandler {
        HeadHandler {}
    }
}

pub(crate) struct TailHandler {}

impl ChannelOutboundHandler for TailHandler {
    fn id(&self) -> String {
        String::from("TAIL")
    }

    fn channel_write(
        &mut self,
        channel_handler_ctx: &mut ChannelOutboundHandlerCtx,
        message: &mut dyn Any,
    ) {
        let bytes = message.downcast_ref::<ByteBuf>();
        match bytes {
            Some(buf) => {
                channel_handler_ctx.channel().write_bytebuf(buf);
            }
            None => {
                println!("TailHandler message is not bytebuf");
            }
        }
    }
}

impl TailHandler {
    pub(crate) fn new() -> TailHandler {
        TailHandler {}
    }
}
