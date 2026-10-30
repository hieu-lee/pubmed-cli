use crate::args::Args;
use crate::http::Ctx;
use crate::model::*;
use crate::ops;
use anyhow::Result;

pub async fn run(ctx: &Ctx, a: &Args, _input: Option<Set>) -> Result<Set> {
    ops::search(ctx, a).await
}
