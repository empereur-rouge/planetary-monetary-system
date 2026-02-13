use crate::client::DagClient;
use crate::error::SimResult;
use crate::metrics::MetricEvent;
use crate::types::WalletInfo;
use tokio::sync::mpsc;

/// Bootstrap funder: claims CUBEs then burns them for PMS (production-like flow)
pub struct Funder;

impl Funder {
    pub fn new() -> Self {
        Self
    }

    /// Fund a single agent: claim CUBEs → burn for PMS
    pub async fn fund(
        &self,
        client: &DagClient,
        wallet: &WalletInfo,
        cubes_to_burn: &str,
        metrics_tx: &mpsc::UnboundedSender<MetricEvent>,
        agent_name: &str,
    ) -> SimResult<()> {
        let addr = &wallet.address;

        // Step 1: Claim CUBEs
        let claim_resp = client.cube_claim(addr).await?;
        let claim_block = claim_resp.data.block_id.unwrap_or_default();
        let cubes_received = claim_resp.data.amount.unwrap_or_default();
        tracing::info!(
            "[{}] Claimed {} CUBE -> block {}",
            agent_name,
            cubes_received,
            &claim_block[..16.min(claim_block.len())]
        );

        // Small delay to let UTXO settle
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Step 2: Burn CUBEs for PMS
        let burn_resp = client
            .cube_burn(&wallet.private_key_b64, cubes_to_burn)
            .await?;
        let burn_block = burn_resp.data.block_id.unwrap_or_default();
        let pms_received = burn_resp.data.pms_received.unwrap_or_default();
        tracing::info!(
            "[{}] Burned {} CUBE -> {} PMS (block {})",
            agent_name,
            cubes_to_burn,
            pms_received,
            &burn_block[..16.min(burn_block.len())]
        );

        let _ = metrics_tx.send(MetricEvent::AgentFunded {
            agent_name: agent_name.to_string(),
            amount: pms_received,
        });

        Ok(())
    }

    /// Fund all agents sequentially: claim CUBEs → burn for PMS
    pub async fn fund_all(
        &self,
        client: &DagClient,
        agents: &[(String, WalletInfo)], // (name, wallet)
        cubes_to_burn: &str,
        metrics_tx: &mpsc::UnboundedSender<MetricEvent>,
    ) -> SimResult<()> {
        for (name, wallet) in agents {
            self.fund(client, wallet, cubes_to_burn, metrics_tx, name)
                .await?;
            // Delay between agents to avoid UTXO contention
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        Ok(())
    }
}
