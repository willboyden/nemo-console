import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
import { EVALUATOR } from '../run-descriptors.js';
// NeMo Evaluator: offline benchmark suites. Scores (bench, model) are a table, not a time series.
const DESC = { system: 'evaluator', charts: [], tables: ['ev.score'], runHistory: EVALUATOR };
class NcEvaluator extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-evaluator', NcEvaluator);
