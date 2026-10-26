// Shared wording and markers for allowance alerts. The rules themselves live in
// src-tauri/src/forecast.rs (pace) and src-tauri/src/compact.rs (fallback); see issue #4.
export type Alert = 'warning' | 'critical' | 'surge';
export type AlertReason = 'low' | 'pace' | 'surge' | 'restricted';

export interface AlertFacts {
  alert: Alert | null;
  alertReason: AlertReason | null;
  runsOutAt: number | null;
  resetsAt: number | null;
  surgeRatio?: number | null;
}

interface WindowForecast {
  provider: string; unit: string; windowId?: string | null;
  alert?: Alert | null; alertReason?: AlertReason | null; runsOutAt?: number | null; surgeRatio?: number | null;
}

const bolt = '<svg viewBox="0 0 10 10" aria-hidden="true"><path d="M6 0L1 6h3l-1 4 5-6H5z" fill="currentColor"/></svg>';

// Symbols repeat what the colors say, for people who can't tell amber from red.
export function alertMark(alert: Alert | null) {
  if (!alert) return '';
  return `<span class="alert-mark ${alert}" aria-hidden="true">${alert === 'surge' ? bolt : alert === 'warning' ? '?' : '!'}</span>`;
}

export function alertSentence(facts: AlertFacts, when: (seconds: number) => string) {
  const { alert, alertReason, runsOutAt, resetsAt } = facts;
  if (!alert || alertReason === 'restricted') return '';
  if (alertReason === 'low') return alert === 'critical' ? 'Under 5% left.' : 'Under 20% left.';
  const runsOut = runsOutAt == null ? 'runs out' : `runs out ${when(runsOutAt)}`;
  const reset = resetsAt == null ? 'the reset' : `the ${when(resetsAt)} reset`;
  if (alertReason === 'surge') {
    const ratio = facts.surgeRatio == null ? 'far above' : `at ${facts.surgeRatio.toFixed(1)}×`;
    return `Recent use is running ${ratio} this window's average. At that pace, it ${runsOut}, before ${reset}.`;
  }
  return `At this window's average pace, it ${runsOut}, ${alert === 'critical' ? 'well ' : ''}before ${reset}.`;
}

// The dashboard's version of compact.rs assess(): the window's forecast alert, a blocked window, or
// the remaining-percent fallback when no forecast covers it.
export function windowAlert(provider: string, windowId: string | null, remaining: number | null, resetsAt: number | null, restricted: boolean, forecasts: WindowForecast[]): AlertFacts {
  const none: AlertFacts = { alert: null, alertReason: null, runsOutAt: null, resetsAt };
  if (restricted) return { ...none, alert: 'critical', alertReason: 'restricted' };
  if (remaining == null) return none;
  const forecast = forecasts.find(row => row.provider === provider && row.unit === 'percentage points' && (row.windowId ?? null) === windowId);
  if (!forecast) return remaining < 5 ? { ...none, alert: 'critical', alertReason: 'low' } : remaining < 20 ? { ...none, alert: 'warning', alertReason: 'low' } : none;
  if (!forecast.alert) return none;
  return { alert: forecast.alert, alertReason: forecast.alertReason ?? null, runsOutAt: forecast.runsOutAt ?? null, resetsAt, surgeRatio: forecast.surgeRatio ?? null };
}
