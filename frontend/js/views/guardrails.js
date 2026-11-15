import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
import { GUARDRAILS } from '../run-descriptors.js';
import { guardrailsExtras } from '../guardrails/extras.js';
import { guardrailsDetail } from '../runviz/guardrails-detail.js';
// NeMo Guardrails: rails server. A playground tab appears only if the backend offers a chat spec. Since addendum 9.16: on Status, the rail pipeline of each served config and an up / down strip
// (gr.up history); on History, a probe matrix (expected vs actual per probe per check run) and a latency strip under the run history built from the same `check` events.
const DESC = {
  system: 'guardrails',
  playground: true,
  charts: [], // the adapter reports only gr.up and gr.configs: gr.up is the strip below, gr.configs a tile
  extras: (scope, shared) => guardrailsExtras(scope, shared).el,
  runHistory: { ...GUARDRAILS, detail: guardrailsDetail },
};
class NcGuardrails extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-guardrails', NcGuardrails);
