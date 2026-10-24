// Route table (pure data, no DOM) so tests can import it. `group` is a nav section; routes without one sit at the top.
export const ROUTES = [
  { path: '/', tag: 'nc-overview', mod: 'overview', nav: 'overview', icon: 'overview', hidden: true },
  { path: '/overview', tag: 'nc-overview', mod: 'overview', nav: 'overview', icon: 'overview' },
  { path: '/switchyard', tag: 'nc-switchyard', mod: 'switchyard', nav: 'switchyard', icon: 'switchyard', group: 'routing' },
  { path: '/relay', tag: 'nc-relay', mod: 'relay', nav: 'relay', icon: 'relay', group: 'routing' },
  { path: '/dynamo', tag: 'nc-dynamo', mod: 'dynamo', nav: 'dynamo', icon: 'dynamo', group: 'engines' },
  { path: '/dcgm', tag: 'nc-dcgm', mod: 'dcgm', nav: 'dcgm', icon: 'gpu', group: 'engines' },
  { path: '/aiperf', tag: 'nc-aiperf', mod: 'aiperf', nav: 'aiperf', icon: 'gauge', group: 'engines' },
  { path: '/guardrails', tag: 'nc-guardrails', mod: 'guardrails', nav: 'guardrails', icon: 'shield', group: 'safety' },
  { path: '/openshell', tag: 'nc-openshell', mod: 'openshell', nav: 'openshell', icon: 'sandbox', group: 'safety' },
  { path: '/skillevaluator', tag: 'nc-skillevaluator', mod: 'skillevaluator', nav: 'skillevaluator', icon: 'skills', group: 'quality' },
  { path: '/evaluator', tag: 'nc-evaluator', mod: 'evaluator', nav: 'evaluator', icon: 'flask', group: 'quality' },
  { path: '/datadesigner', tag: 'nc-datadesigner', mod: 'datadesigner', nav: 'datadesigner', icon: 'skills', group: 'quality' },
  { path: '/audit', tag: 'nc-audit', mod: 'audit', nav: 'audit', icon: 'audit', admin: true, group: 'console' },
  { path: '/settings', tag: 'nc-settings', mod: 'settings', nav: 'settings', icon: 'settings', group: 'console' },
];
