// src/robotics_alignment.rs

use crate::{AgentAction, SafetyContract};
use crate::action_policy::UnstructuredTextParser;
use crate::action_filter::{ActionFilter, FilterOutput};
use crate::ros2_adapter::{Ros2Adapter, Ros2TwistMessage, Vector3D};
use crate::kirra_core::KirraKernelGovernor;

pub struct AlignmentBridge<C: SafetyContract + Copy> {
    parser: UnstructuredTextParser,
    filter: ActionFilter<C>,
    ros2_adapter: Ros2Adapter,
}

impl<C: SafetyContract + Copy> AlignmentBridge<C> {
    pub fn new(contract: C) -> Self {
        let angular_limit = contract.max_angular_rate();
        let ros2_adapter = Ros2Adapter::new(angular_limit)
            .unwrap_or(Ros2Adapter { angular_velocity_limit: 1.5 });
        Self {
            parser: UnstructuredTextParser,
            filter: ActionFilter::new(contract),
            ros2_adapter,
        }
    }

    pub fn align_and_serialize_intent(&self, raw_json: &str) -> Result<(FilterOutput, Vec<u8>), &'static str> {
        let action = self.parser.parse_llm_json_intent(raw_json)?;
        let contract = self.filter.contract;
        let mut gov = KirraKernelGovernor::new(contract, 0.0, contract.min_bound(), contract.max_bound());
        let output = self.filter.process_agent_intent(&mut gov, action, 1.0);

        let (linear_x, angular_z) = match &output.sanitized_action {
            AgentAction::MoveLinear { velocity } => (*velocity, 0.0),
            AgentAction::Rotate { angular_velocity } => (0.0, *angular_velocity),
            _ => (0.0, 0.0),
        };
        let msg = Ros2TwistMessage {
            linear: Vector3D { x: linear_x, y: 0.0, z: 0.0 },
            angular: Vector3D { x: 0.0, y: 0.0, z: angular_z },
        };
        let frame = self.ros2_adapter.encode_twist_frame(&msg);

        Ok((output, frame))
    }
}
