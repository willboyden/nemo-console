import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
import { GUARDRAILS } from '../run-descriptors.js';
// NeMo Guardrails: rails server. A playground tab appears only if the backend offers a chat spec.
const DESC = { system: 'guardrails', playground: true, charts: [], runHistory: GUARDRAILS }; // the adapter reports only gr.up and gr.configs; the History tab reads the `check` runs
class NcGuardrails extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-guardrails', NcGuardrails);
