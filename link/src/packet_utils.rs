use crate::packets::{Command, Response};
use crate::protos::{AuthResponse, SubscribeResponse, UnsubscribeResponse};
use anyhow::Context;
use prost::Message;

impl TryFrom<Response> for SubscribeResponse {
    type Error = anyhow::Error;

    fn try_from(response: Response) -> std::result::Result<Self, Self::Error> {
        match response.command {
            Command::CmdSubAllType
            | Command::CmdSubOrderBook
            | Command::CmdSubTrades
            | Command::CmdSubBrokers
            | Command::CmdSubDepth
            | Command::CmdSubMarketInfo => Ok(SubscribeResponse::decode(response.body.as_ref())
                .context("decode response body err")?),
            _ => Err(anyhow::anyhow!(
                "unsupported command: {:?}",
                response.command
            )),
        }
    }
}

impl TryFrom<Response> for AuthResponse {
    type Error = anyhow::Error;

    fn try_from(response: Response) -> Result<Self, Self::Error> {
        match response.command {
            Command::Auth => Ok(AuthResponse::decode(response.body.as_ref())
                .context("decode auth response body err")?),
            _ => Err(anyhow::anyhow!(
                "response packet'command is not Auth: {:?}",
                response.command
            )),
        }
    }
}

impl TryFrom<Response> for UnsubscribeResponse {
    type Error = anyhow::Error;

    fn try_from(response: Response) -> Result<Self, Self::Error> {
        match response.command {
            Command::CmdUnSubAllType
            | Command::CmdUnSubOrderBook
            | Command::CmdUnSubDepth
            | Command::CmdUnSubBrokers => Ok(UnsubscribeResponse::decode(response.body.as_ref())
                .context("decode unsubscribe response body err")?),
            _ => Err(anyhow::anyhow!(
                "response packet'command is not Unsubscribe: {:?}",
                response.command
            )),
        }
    }
}
