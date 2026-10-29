// The eleven managed systems (domain SystemId, ARCHITECTURE.md 9.7, 9.8 and 10.9). Pure data: the audit filter, nav and tests read it.
export const SYSTEM_IDS = ['switchyard', 'relay', 'dynamo', 'skillevaluator', 'datadesigner', 'openshell', 'dcgm', 'aiperf', 'guardrails', 'evaluator', 'modelopt'];
// The six systems added in 9.8 and 10.9 render through the descriptor-driven generic view.
export const SIX_IDS = ['openshell', 'dcgm', 'aiperf', 'guardrails', 'evaluator', 'modelopt'];
// The five systems added in 9.8 (kept for the tests that still name them).
export const FIVE_IDS = SIX_IDS.filter((id) => id !== 'modelopt');
