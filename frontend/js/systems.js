// The twelve managed systems (domain SystemId, ARCHITECTURE.md 9.7, 9.8, 10.9 and 10.10). Pure data: the audit filter, nav and tests read it.
export const SYSTEM_IDS = ['switchyard', 'relay', 'dynamo', 'skillevaluator', 'datadesigner', 'openshell', 'dcgm', 'aiperf', 'guardrails', 'evaluator', 'modelopt', 'gym'];
// The seven systems added in 9.8, 10.9 and 10.10 render through the descriptor-driven generic view.
export const SEVEN_IDS = ['openshell', 'dcgm', 'aiperf', 'guardrails', 'evaluator', 'modelopt', 'gym'];
// The six of 9.8 and 10.9, and the five of 9.8 (kept for the tests that still name them).
export const SIX_IDS = SEVEN_IDS.filter((id) => id !== 'gym');
export const FIVE_IDS = SIX_IDS.filter((id) => id !== 'modelopt');
