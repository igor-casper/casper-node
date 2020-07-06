use std::convert::{TryFrom, TryInto};

use node::components::contract_runtime::core::engine_state::upgrade::UpgradeConfig;
use types::ProtocolVersion;

use crate::engine_server::{ipc::UpgradeRequest, mappings::MappingError};

impl TryFrom<UpgradeRequest> for UpgradeConfig {
    type Error = MappingError;

    fn try_from(mut pb_upgrade_request: UpgradeRequest) -> Result<Self, Self::Error> {
        let pre_state_hash = pb_upgrade_request
            .get_parent_state_hash()
            .try_into()
            .map_err(|_| MappingError::InvalidStateHash("pre_state_hash".to_string()))?;

        let current_protocol_version = pb_upgrade_request.take_protocol_version().into();

        let upgrade_point = pb_upgrade_request.mut_upgrade_point();
        let new_protocol_version: ProtocolVersion = upgrade_point.take_protocol_version().into();
        let (upgrade_installer_bytes, upgrade_installer_args) =
            if !upgrade_point.has_upgrade_installer() {
                (None, None)
            } else {
                let upgrade_installer = upgrade_point.take_upgrade_installer();
                let bytes = upgrade_installer.code;
                let bytes = if bytes.is_empty() { None } else { Some(bytes) };
                let args = upgrade_installer.args;
                let args = if args.is_empty() { None } else { Some(args) };
                (bytes, args)
            };

        let wasm_costs = if !upgrade_point.has_new_costs() {
            None
        } else {
            Some(upgrade_point.mut_new_costs().take_wasm().into())
        };
        let activation_point = if !upgrade_point.has_activation_point() {
            None
        } else {
            Some(upgrade_point.get_activation_point().rank)
        };

        Ok(UpgradeConfig::new(
            pre_state_hash,
            current_protocol_version,
            new_protocol_version,
            upgrade_installer_args,
            upgrade_installer_bytes,
            wasm_costs,
            activation_point,
        ))
    }
}
