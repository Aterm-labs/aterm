// Pro licensing for Agent Sessions — Lemon Squeezy license keys.
//
// Open-core model: the Community edition is free; a few features (parallel
// comparison, launch templates) are gated behind a Pro licence. This module
// owns that gate: a 14-day trial plus online validation of a Lemon Squeezy
// license key (its public License API needs no secret/API key — only the
// license key itself), with a cached result so the gate stays synchronous and
// keeps working offline.
//
// CONFIGURA antes de publicar:
//   - BUY_URL_ANNUAL / BUY_URL_MONTHLY: enlaces de checkout de Lemon Squeezy.
//   - PRODUCT_ID (opcional): restringe a tu producto (meta.product_id de LS).

import * as vscode from "vscode";

/** Trial length in days from first activation. */
const TRIAL_DAYS = 14;

/** Lemon Squeezy checkout links per plan. */
export const BUY_URL_ANNUAL =
  "https://aterm.lemonsqueezy.com/checkout/buy/258755f8-8c93-41ab-b0b0-e8d07fdfcc25";
export const BUY_URL_MONTHLY =
  "https://aterm.lemonsqueezy.com/checkout/buy/87d06b1a-b038-434d-9ad3-b58553f4a4ea";

/** Let the user pick a plan, then open its checkout. */
export async function openBuy(): Promise<void> {
  const pick = await vscode.window.showQuickPick(
    [
      {
        label: "$(calendar) Plan anual",
        description: "mejor precio",
        url: BUY_URL_ANNUAL,
      },
      { label: "$(clock) Plan mensual", url: BUY_URL_MONTHLY },
    ],
    { placeHolder: "Elige tu plan de Agent Sessions Pro" }
  );
  if (pick) void vscode.env.openExternal(vscode.Uri.parse(pick.url));
}

/** Optional: restrict accepted keys to this Lemon Squeezy product id. Empty =
 *  accept any valid key from any product (less strict). */
const PRODUCT_ID = "";

const LS_API = "https://api.lemonsqueezy.com/v1/licenses";

const TRIAL_START_KEY = "pro.trialStart";
const LICENSE_KEY = "pro.licenseKey";
const INSTANCE_ID_KEY = "pro.instanceId";
const LICENSED_CACHE_KEY = "pro.licensedCache";

type ProStatus = "licensed" | "trial" | "expired";

/** POST to the Lemon Squeezy License API (form-encoded). Returns parsed JSON. */
async function lsPost(
  path: string,
  params: Record<string, string>
): Promise<Record<string, unknown>> {
  const body = Object.entries(params)
    .map(([k, v]) => `${k}=${encodeURIComponent(v)}`)
    .join("&");
  const res = await fetch(`${LS_API}/${path}`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/x-www-form-urlencoded",
    },
    body,
  });
  return (await res.json()) as Record<string, unknown>;
}

function productMatches(data: Record<string, unknown>): boolean {
  if (!PRODUCT_ID) return true;
  const meta = data.meta as { product_id?: unknown } | undefined;
  return String(meta?.product_id ?? "") === PRODUCT_ID;
}

export class LicenseService {
  private readonly statusItem: vscode.StatusBarItem;

  constructor(private readonly ctx: vscode.ExtensionContext) {
    this.statusItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Right,
      99
    );
    this.statusItem.command = "agentSessions.proStatus";
    ctx.subscriptions.push(this.statusItem);
    // Periodic re-validation (every 12 h) so an expiry/refund flips back to
    // Community without needing a restart.
    const timer = setInterval(() => void this.revalidate(), 12 * 3600 * 1000);
    ctx.subscriptions.push({ dispose: () => clearInterval(timer) });
  }

  /** Update the status-bar widget to reflect the current Pro state. */
  refreshStatusBar(): void {
    const st = this.status();
    if (st === "licensed") {
      this.statusItem.text = "$(verified) Pro";
      this.statusItem.tooltip = "Agent Sessions Pro — licencia activa";
      this.statusItem.backgroundColor = undefined;
    } else if (st === "trial") {
      const d = this.trialDaysLeft();
      this.statusItem.text = `$(clock) Pro: prueba ${d}d`;
      this.statusItem.tooltip = `Prueba Pro: ${d} día(s) restantes. Clic para activar o comprar.`;
      this.statusItem.backgroundColor =
        d <= 3
          ? new vscode.ThemeColor("statusBarItem.warningBackground")
          : undefined;
    } else {
      this.statusItem.text = "$(star) Activar Pro";
      this.statusItem.tooltip = "Funciones Pro bloqueadas. Clic para activar o comprar.";
      this.statusItem.backgroundColor = undefined;
    }
    this.statusItem.show();
  }

  /** Once per day, warn when the trial is about to lapse (≤3 days left). */
  maybeWarnTrialExpiring(): void {
    if (this.status() !== "trial") return;
    const d = this.trialDaysLeft();
    if (d > 3) return;
    const today = new Date().toISOString().slice(0, 10);
    if (this.ctx.globalState.get<string>("pro.lastTrialWarn") === today) return;
    void this.ctx.globalState.update("pro.lastTrialWarn", today);
    void vscode.window
      .showWarningMessage(
        `Agent Sessions: tu prueba Pro termina en ${d} día(s). Después, comparativa paralela y plantillas pedirán licencia.`,
        "Comprar Pro",
        "Activar licencia"
      )
      .then((p) => {
        if (p === "Comprar Pro") void openBuy();
        else if (p === "Activar licencia") void this.activate();
      });
  }

  /** Start the trial clock on first run so the grace period is deterministic. */
  startTrialIfNeeded(): void {
    if (!this.ctx.globalState.get<number>(TRIAL_START_KEY)) {
      void this.ctx.globalState.update(TRIAL_START_KEY, Date.now());
    }
  }

  /** Synchronous gate: cached licence state OR active trial. */
  isPro(): boolean {
    return this.cachedLicensed() || this.inTrial();
  }

  status(): ProStatus {
    if (this.cachedLicensed()) return "licensed";
    return this.inTrial() ? "trial" : "expired";
  }

  private cachedLicensed(): boolean {
    return this.ctx.globalState.get<boolean>(LICENSED_CACHE_KEY) === true;
  }

  trialDaysLeft(): number {
    const start = this.ctx.globalState.get<number>(TRIAL_START_KEY);
    if (!start) return TRIAL_DAYS;
    const left = TRIAL_DAYS - Math.floor((Date.now() - start) / 86_400_000);
    return Math.max(0, left);
  }

  private inTrial(): boolean {
    return this.trialDaysLeft() > 0;
  }

  /** Re-check the stored key against Lemon Squeezy and refresh the cache.
   *  Offline-safe: a network error leaves the last known state untouched (so a
   *  legitimate user offline isn't locked out); only an explicit `valid:false`
   *  revokes. Call on startup and after activation. */
  async revalidate(): Promise<void> {
    const key = this.ctx.globalState.get<string>(LICENSE_KEY);
    if (!key) {
      await this.ctx.globalState.update(LICENSED_CACHE_KEY, false);
    } else {
      try {
        const inst = this.ctx.globalState.get<string>(INSTANCE_ID_KEY);
        const data = await lsPost(
          "validate",
          inst ? { license_key: key, instance_id: inst } : { license_key: key }
        );
        const ok = data.valid === true && productMatches(data);
        await this.ctx.globalState.update(LICENSED_CACHE_KEY, ok);
      } catch {
        /* network down — keep the cached state */
      }
    }
    this.refreshStatusBar();
  }

  /** Prompt for a key, activate it with Lemon Squeezy and cache the result. */
  async activate(): Promise<void> {
    const key = await vscode.window.showInputBox({
      title: "Activar licencia Pro",
      prompt: "Pega tu clave de licencia de Lemon Squeezy",
      ignoreFocusOut: true,
      placeHolder: "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX",
    });
    if (key === undefined) return;
    const trimmed = key.trim();
    if (!trimmed) {
      await this.clearLicense();
      vscode.window.showInformationMessage("Agent Sessions: licencia eliminada.");
      return;
    }

    const instanceName = `vscode-${vscode.env.machineId.slice(0, 12)}`;
    try {
      let data = await lsPost("activate", {
        license_key: trimmed,
        instance_name: instanceName,
      });

      // Already activated to its limit on other machines → fall back to a plain
      // validate so a legitimate re-activation isn't blocked.
      if (
        data.activated !== true &&
        /activation limit/i.test(String(data.error ?? ""))
      ) {
        data = await lsPost("validate", { license_key: trimmed });
        if (data.valid === true && productMatches(data)) {
          await this.store(trimmed, undefined);
          vscode.window.showInformationMessage(
            "Agent Sessions: ¡licencia Pro activada! Gracias 🙌"
          );
          return;
        }
      }

      if (data.activated === true) {
        if (!productMatches(data)) {
          vscode.window.showErrorMessage(
            "Agent Sessions: esa clave no corresponde a este producto."
          );
          return;
        }
        const instance = data.instance as { id?: unknown } | undefined;
        await this.store(
          trimmed,
          instance?.id != null ? String(instance.id) : undefined
        );
        vscode.window.showInformationMessage(
          "Agent Sessions: ¡licencia Pro activada! Gracias 🙌"
        );
        return;
      }

      const pick = await vscode.window.showErrorMessage(
        `Agent Sessions: la clave no es válida (${String(data.error ?? "rechazada")}).`,
        "Comprar Pro"
      );
      if (pick === "Comprar Pro") await openBuy();
    } catch (e) {
      vscode.window.showErrorMessage(
        `Agent Sessions: no se pudo contactar con el servidor de licencias (${(e as Error).message}).`
      );
    }
  }

  private async store(key: string, instanceId: string | undefined): Promise<void> {
    await this.ctx.globalState.update(LICENSE_KEY, key);
    await this.ctx.globalState.update(INSTANCE_ID_KEY, instanceId);
    await this.ctx.globalState.update(LICENSED_CACHE_KEY, true);
    this.refreshStatusBar();
  }

  private async clearLicense(): Promise<void> {
    await this.ctx.globalState.update(LICENSE_KEY, undefined);
    await this.ctx.globalState.update(INSTANCE_ID_KEY, undefined);
    await this.ctx.globalState.update(LICENSED_CACHE_KEY, false);
    this.refreshStatusBar();
  }

  // ── QA helpers (para probar el gating sin esperar 14 días) ───────────────
  async expireTrialForTesting(): Promise<void> {
    await this.ctx.globalState.update(
      TRIAL_START_KEY,
      Date.now() - (TRIAL_DAYS + 1) * 86_400_000
    );
    await this.clearLicense();
  }
  async resetTrialForTesting(): Promise<void> {
    await this.ctx.globalState.update(TRIAL_START_KEY, Date.now());
    await this.clearLicense();
  }

  /** Dev-only: pick a Pro state to test the gate. */
  async debug(): Promise<void> {
    const pick = await vscode.window.showQuickPick(
      [
        { label: "$(error) Caducar prueba ahora (bloquear Pro)", action: "expire" },
        { label: "$(history) Reiniciar prueba (14 días)", action: "reset" },
        { label: "$(key) Activar licencia…", action: "activate" },
      ],
      { placeHolder: `Estado Pro actual: ${this.status()} · ${this.trialDaysLeft()} día(s)` }
    );
    if (!pick) return;
    if (pick.action === "expire") {
      await this.expireTrialForTesting();
      vscode.window.showInformationMessage("Pro: prueba caducada. Las funciones Pro están bloqueadas.");
    } else if (pick.action === "reset") {
      await this.resetTrialForTesting();
      vscode.window.showInformationMessage("Pro: prueba reiniciada (14 días).");
    } else {
      await this.activate();
    }
  }

  /** Show current Pro status and offer the relevant action. */
  async showStatus(): Promise<void> {
    const st = this.status();
    if (st === "licensed") {
      const pick = await vscode.window.showInformationMessage(
        "Agent Sessions Pro: licencia activa. ✅",
        "Revalidar"
      );
      if (pick === "Revalidar") await this.revalidate();
      return;
    }
    const msg =
      st === "trial"
        ? `Agent Sessions Pro: prueba activa, ${this.trialDaysLeft()} día(s) restantes.`
        : "Agent Sessions Pro: la prueba ha terminado. Las funciones Pro están bloqueadas.";
    const pick = await vscode.window.showInformationMessage(
      msg,
      "Activar licencia",
      "Comprar Pro",
      "Revalidar"
    );
    if (pick === "Activar licencia") await this.activate();
    else if (pick === "Comprar Pro") await openBuy();
    else if (pick === "Revalidar") await this.revalidate();
  }
}
