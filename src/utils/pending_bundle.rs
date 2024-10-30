use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use futures::stream::{Stream, StreamExt};
use futures::TryFutureExt;
use tokio::time;
use tokio_stream::wrappers::IntervalStream;
use thiserror::Error;
use pin_project::pin_project;
use alloy::hex;
use alloy::primitives::{FixedBytes, TxHash, B256, U64};
use alloy::providers::Provider;
use alloy::rpc::types::{Block, BlockTransactions, BlockTransactionsKind};


#[pin_project]
pub struct PendingBundle<'a, P, N> {
    pub bundle_hash: Option<B256>,
    pub block: U64,
    pub transactions: Vec<TxHash>,
    provider: &'a dyn Provider<P, N>,
    state: PendingBundleState<'a>,
    interval: Box<dyn Stream<Item = ()> + Send + Unpin>,
}

impl<'a, P, N> PendingBundle<'a, P, N>
where
    P: Clone + alloy::transports::Transport + Send + Sync + 'static,
    N: alloy::providers::Network<BlockResponse = Block<FixedBytes<32>>> + Send + Sync + 'static,
{
    pub fn new(
        bundle_hash: Option<B256>,
        block: U64,
        transactions: Vec<TxHash>,
        provider: &'a dyn Provider<P, N>,
    ) -> Self {
        let interval = IntervalStream::new(
            time::interval(
                Duration::from_millis(7000)
            )).map(|_| ()
        );
        Self {
            bundle_hash,
            block,
            transactions,
            provider,
            state: PendingBundleState::PausedGettingBlock,
            interval: Box::new(interval),
        }
    }

    /// Get the bundle hash for this pending bundle.
    #[deprecated(note = "use the bundle_hash field instead")]
    pub fn bundle_hash(&self) -> Option<B256> {
        self.bundle_hash
    }
}

impl<'a, P, N> Future for PendingBundle<'a, P, N>
where
    P: Clone + alloy::transports::Transport + Send + Sync + 'static,
    N: alloy::providers::Network<BlockResponse = Block<FixedBytes<32>>> + Send + Sync + 'static,
{
    type Output = Result<Option<B256>, PendingBundleError>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        let this = self.project();

        match this.state {
            PendingBundleState::PausedGettingBlock => {
                futures::ready!(this.interval.poll_next_unpin(ctx));

                let fut = Box::pin(
                    this.provider
                        .get_block((*this.block).into(), BlockTransactionsKind::Full)
                        .map_err(|e| ProviderError::CustomError(format!("RPC error: {:?}", e))),
                );
                
                *this.state = PendingBundleState::GettingBlock(fut);
                ctx.waker().wake_by_ref();
            }
            PendingBundleState::GettingBlock(fut) => {
                let block_res = futures::ready!(fut.as_mut().poll(ctx));

                if block_res.is_err() {
                    *this.state = PendingBundleState::PausedGettingBlock;
                    ctx.waker().wake_by_ref();
                    return Poll::Pending;
                }

                let block_opt = block_res.unwrap();
                if block_opt.is_none() {
                    *this.state = PendingBundleState::PausedGettingBlock;
                    ctx.waker().wake_by_ref();
                    return Poll::Pending;
                }

                let block = block_opt.unwrap();
                if block.size.is_none() {
                    *this.state = PendingBundleState::PausedGettingBlock;
                    ctx.waker().wake_by_ref();
                    return Poll::Pending;
                }

                let included: bool = match &block.transactions {
                    BlockTransactions::Full(txs) => {
                        this.transactions.iter().all(|tx_hash| txs.contains(tx_hash))
                    }
                    BlockTransactions::Hashes(hashes) => {
                        this.transactions.iter().all(|tx_hash| hashes.contains(tx_hash))
                    }
                    BlockTransactions::Uncle => {
                        false
                    }
                };
                

                *this.state = PendingBundleState::Completed;
                if included {
                    return Poll::Ready(Ok(*this.bundle_hash));
                } else {
                    return Poll::Ready(Err(PendingBundleError::BundleNotIncluded));
                }
            }
            PendingBundleState::Completed => {
                panic!("polled pending bundle future after completion")
            }
        }

        Poll::Pending
    }
}


/// Errors for pending bundles.
#[derive(Error, Debug)]
pub enum PendingBundleError {
    /// The bundle was not included in the target block.
    #[error("Bundle was not included in target block")]
    BundleNotIncluded,
    /// An error occurred while interacting with the RPC endpoint.
    #[error(transparent)]
    ProviderError(#[from] ProviderError),
}



type PinBoxFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

enum PendingBundleState<'a> {
    /// Waiting for an interval before calling API again
    PausedGettingBlock,

    /// Polling the blockchain to get block information
    GettingBlock(PinBoxFut<'a, Option<Block<TxHash>>>),

    /// Future has completed
    Completed,
}

#[derive(Debug, Error)]
/// An error thrown when making a call to the provider
pub(crate) enum ProviderError {
    /// Error in underlying lib `serde_json`
    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),

    /// Error in underlying lib `hex`
    #[error(transparent)]
    HexError(#[from] hex::FromHexError),

    /// Error in underlying lib `reqwest`
    #[error(transparent)]
    HTTPError(#[from] reqwest::Error),

    /// Custom error from unknown source
    #[error("custom error: {0}")]
    CustomError(String),
}
