// The thirteen managed systems (domain SystemId, ARCHITECTURE.md 9.7, 9.8, 10.9 and 10.10; NVIDIA Flare since 0.10.0, ADR-0020). Pure data: the audit filter, nav and tests read it.
export const SYSTEM_IDS = ['switchyard', 'relay', 'dynamo', 'skillevaluator', 'datadesigner', 'openshell', 'dcgm', 'aiperf', 'guardrails', 'evaluator', 'modelopt', 'gym', 'flare'];
// The systems that render through the descriptor-driven generic view: the seven added in 9.8, 10.9 and 10.10, and
// NVIDIA Flare (0.10.0). The name is kept for the tests and modules that import it.
export const SEVEN_IDS = ['openshell', 'dcgm', 'aiperf', 'guardrails', 'evaluator', 'modelopt', 'gym', 'flare'];
// The six of 9.8 and 10.9, and the five of 9.8 (kept for the tests that still name them).
export const SIX_IDS = SEVEN_IDS.filter((id) => id !== 'gym' && id !== 'flare');
export const FIVE_IDS = SIX_IDS.filter((id) => id !== 'modelopt');
