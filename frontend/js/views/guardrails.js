import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
// NeMo Guardrails: rails server. A playground tab appears only if the backend offers a chat spec.
const DESC = { system: 'guardrails', playground: true, charts: [] };   // the adapter reports only gr.up and gr.configs
class NcGuardrails extends NcView {
  setup() { genericSystemView(this, DESC); }
}
customElements.define('nc-guardrails', NcGuardrails);
