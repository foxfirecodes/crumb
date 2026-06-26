import { useEffect, useMemo, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  getAppSettings,
  getLaunchAtLogin,
  saveAppSettings,
  setLaunchAtLogin,
  testAiSettings,
  testDiscordSettings,
} from "./lib/ipc";
import type { AcpConnector, AppSettings, SettingsTestResult } from "./lib/types";

const DEFAULT_CLAUDE_COMMAND =
  "npx -y @agentclientprotocol/claude-agent-acp@0.33.1";
const DEFAULT_CODEX_COMMAND = "npx -y @agentclientprotocol/codex-acp@0.0.44";

const CONNECTORS: { value: AcpConnector; label: string }[] = [
  { value: "claudeCode", label: "Claude Code" },
  { value: "codex", label: "Codex" },
  { value: "custom", label: "Custom" },
];

const EMPTY_SETTINGS: AppSettings = {
  discordAppId: "",
  discordBotToken: "",
  discordUserToken: "",
  acpConnector: "claudeCode",
  claudeCode: {
    model: "sonnet",
    effort: "low",
    configDir: "",
    command: "",
  },
  codex: {
    model: "gpt-5.4-mini",
    effort: "low",
    command: "",
  },
  customAcp: {
    command: "",
    env: "",
    sessionMeta: "",
  },
  keepPopoverOpenOnView: false,
};

type SaveState = "idle" | "saving" | "saved" | "error";

const settingsMessageClassName = (ok: boolean) =>
  ok
    ? "settings-message settings-message--ok"
    : "settings-message settings-message--error";

export default function SettingsApp() {
  const [settings, setSettings] = useState<AppSettings>(EMPTY_SETTINGS);
  const [savedDiscordAppId, setSavedDiscordAppId] = useState("");
  const [launchAtLogin, setLaunchAtLoginState] = useState(false);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [message, setMessage] = useState<string | null>(null);
  const [discordTestResult, setDiscordTestResult] =
    useState<SettingsTestResult | null>(null);
  const [aiTestResult, setAiTestResult] =
    useState<SettingsTestResult | null>(null);
  const [testing, setTesting] = useState<"discord" | "ai" | null>(null);

  useEffect(() => {
    getAppSettings()
      .then((loaded) => {
        setSettings(loaded);
        setSavedDiscordAppId(loaded.discordAppId.trim());
      })
      .catch((error) => {
        console.error(error);
        setMessage(String(error));
        setSaveState("error");
      });
    getLaunchAtLogin()
      .then(setLaunchAtLoginState)
      .catch((error) => console.error(error));
  }, []);

  const canSave = useMemo(() => saveState !== "saving", [saveState]);
  const installDiscordAppId =
    settings.discordAppId.trim() === savedDiscordAppId
      ? savedDiscordAppId
      : "";

  const update = <K extends keyof AppSettings>(
    key: K,
    value: AppSettings[K],
  ) => {
    setSettings((current) => ({ ...current, [key]: value }));
    setSaveState("idle");
    setMessage(null);
    setDiscordTestResult(null);
    setAiTestResult(null);
  };

  const updateConnector = (connector: AcpConnector) => {
    update("acpConnector", connector);
  };

  const updateNested = <
    Section extends "claudeCode" | "codex" | "customAcp",
  >(
    section: Section,
    key: keyof AppSettings[Section],
    value: string,
  ) => {
    setSettings((current) => ({
      ...current,
      [section]: {
        ...current[section],
        [key]: value,
      },
    }));
    setSaveState("idle");
    setMessage(null);
    setDiscordTestResult(null);
    setAiTestResult(null);
  };

  const save = () => {
    setSaveState("saving");
    setMessage(null);
    saveAppSettings(settings)
      .then((saved) => {
        setSettings(saved);
        setSavedDiscordAppId(saved.discordAppId.trim());
        setSaveState("saved");
        setMessage("Settings saved.");
      })
      .catch((error) => {
        console.error(error);
        setSaveState("error");
        setMessage(String(error));
      });
  };

  const runDiscordTest = () => {
    setTesting("discord");
    setDiscordTestResult(null);
    testDiscordSettings(settings)
      .then(setDiscordTestResult)
      .catch((error) =>
        setDiscordTestResult({ ok: false, message: String(error) }),
      )
      .finally(() => setTesting(null));
  };

  const installDiscordApp = () => {
    if (!installDiscordAppId) return;
    openUrl(
      `https://discord.com/oauth2/authorize?client_id=${encodeURIComponent(
        installDiscordAppId,
      )}&scope=applications.commands&integration_type=1`,
    ).catch(console.error);
  };

  const runAiTest = () => {
    setTesting("ai");
    setAiTestResult(null);
    testAiSettings(settings)
      .then(setAiTestResult)
      .catch((error) =>
        setAiTestResult({ ok: false, message: String(error) }),
      )
      .finally(() => setTesting(null));
  };

  const toggleLaunchAtLogin = (enabled: boolean) => {
    setLaunchAtLoginState(enabled);
    setLaunchAtLogin(enabled)
      .then(setLaunchAtLoginState)
      .catch((error) => {
        console.error(error);
        setLaunchAtLoginState(!enabled);
        setSaveState("error");
        setMessage(String(error));
      });
  };

  return (
    <div className="settings-window">
      <header className="settings-window__header">
        <div>
          <h1>Crumb Settings</h1>
        </div>
        <button
          className="settings-window__primary"
          onClick={save}
          disabled={!canSave}
        >
          {saveState === "saving" ? "Saving..." : "Save"}
        </button>
      </header>

      <main className="settings-window__body">
        {message && (
          <div
            className={
              saveState === "error"
                ? "settings-message settings-message--error"
                : "settings-message settings-message--ok"
            }
          >
            {message}
          </div>
        )}

        <section className="settings-section">
          <h2>Discord</h2>
          {discordTestResult && (
            <div className={settingsMessageClassName(discordTestResult.ok)}>
              {discordTestResult.message}
            </div>
          )}
          <label className="settings-field">
            <span>Application ID</span>
            <input
              value={settings.discordAppId}
              onChange={(e) => update("discordAppId", e.target.value)}
              placeholder="000000000000000000"
            />
          </label>
          <label className="settings-field">
            <span>Bot token</span>
            <input
              type="password"
              value={settings.discordBotToken}
              onChange={(e) => update("discordBotToken", e.target.value)}
            />
          </label>
          <label className="settings-field">
            <span>User token</span>
            <input
              type="password"
              value={settings.discordUserToken}
              onChange={(e) => update("discordUserToken", e.target.value)}
            />
          </label>
          <div className="settings-actions">
            <button
              className="settings-window__secondary"
              onClick={runDiscordTest}
              disabled={testing !== null}
            >
              {testing === "discord" ? "Testing..." : "Test Discord"}
            </button>
            {installDiscordAppId && (
              <button
                className="settings-window__secondary"
                onClick={installDiscordApp}
              >
                Install App
              </button>
            )}
          </div>
        </section>

        <section className="settings-section">
          <h2>AI Extraction</h2>
          {aiTestResult && (
            <div className={settingsMessageClassName(aiTestResult.ok)}>
              {aiTestResult.message}
            </div>
          )}
          <div className="settings-segmented" role="tablist">
            {CONNECTORS.map((connector) => (
              <button
                key={connector.value}
                type="button"
                className={
                  settings.acpConnector === connector.value
                    ? "settings-segmented__option settings-segmented__option--active"
                    : "settings-segmented__option"
                }
                onClick={() => updateConnector(connector.value)}
              >
                {connector.label}
              </button>
            ))}
          </div>

          {settings.acpConnector === "claudeCode" && (
            <>
              <div className="settings-grid">
                <label className="settings-field">
                  <span>Model</span>
                  <select
                    value={settings.claudeCode.model}
                    onChange={(e) =>
                      updateNested("claudeCode", "model", e.target.value)
                    }
                  >
                    <option value="sonnet">sonnet</option>
                    <option value="haiku">haiku</option>
                  </select>
                </label>
                <label className="settings-field">
                  <span>Effort</span>
                  <select
                    value={settings.claudeCode.effort}
                    onChange={(e) =>
                      updateNested("claudeCode", "effort", e.target.value)
                    }
                  >
                    <option value="low">low</option>
                    <option value="medium">medium</option>
                    <option value="high">high</option>
                    <option value="xhigh">xhigh</option>
                  </select>
                </label>
              </div>
              <label className="settings-field">
                <span>Claude config dir</span>
                <input
                  value={settings.claudeCode.configDir}
                  onChange={(e) =>
                    updateNested("claudeCode", "configDir", e.target.value)
                  }
                  placeholder="~/.claude"
                />
              </label>
              <label className="settings-field">
                <span>ACP command</span>
                <input
                  value={settings.claudeCode.command}
                  onChange={(e) =>
                    updateNested("claudeCode", "command", e.target.value)
                  }
                  placeholder={DEFAULT_CLAUDE_COMMAND}
                />
              </label>
            </>
          )}

          {settings.acpConnector === "codex" && (
            <>
              <div className="settings-grid">
                <label className="settings-field">
                  <span>Model</span>
                  <select
                    value={settings.codex.model}
                    onChange={(e) =>
                      updateNested("codex", "model", e.target.value)
                    }
                  >
                    <option value="gpt-5.4-mini">gpt-5.4-mini</option>
                    <option value="gpt-5.4">gpt-5.4</option>
                    <option value="gpt-5.5">gpt-5.5</option>
                  </select>
                </label>
                <label className="settings-field">
                  <span>Effort</span>
                  <select
                    value={settings.codex.effort}
                    onChange={(e) =>
                      updateNested("codex", "effort", e.target.value)
                    }
                  >
                    <option value="low">low</option>
                    <option value="medium">medium</option>
                    <option value="high">high</option>
                    <option value="xhigh">xhigh</option>
                  </select>
                </label>
              </div>
              <label className="settings-field">
                <span>ACP command</span>
                <input
                  value={settings.codex.command}
                  onChange={(e) =>
                    updateNested("codex", "command", e.target.value)
                  }
                  placeholder={DEFAULT_CODEX_COMMAND}
                />
              </label>
            </>
          )}

          {settings.acpConnector === "custom" && (
            <>
              <label className="settings-field">
                <span>ACP command</span>
                <input
                  value={settings.customAcp.command}
                  onChange={(e) =>
                    updateNested("customAcp", "command", e.target.value)
                  }
                  placeholder="my-acp-agent"
                />
              </label>
              <label className="settings-field">
                <span>Environment</span>
                <textarea
                  value={settings.customAcp.env}
                  onChange={(e) =>
                    updateNested("customAcp", "env", e.target.value)
                  }
                  placeholder="KEY=value"
                />
              </label>
              <label className="settings-field">
                <span>Session metadata JSON</span>
                <textarea
                  value={settings.customAcp.sessionMeta}
                  onChange={(e) =>
                    updateNested("customAcp", "sessionMeta", e.target.value)
                  }
                  placeholder='{"disableBuiltInTools":true}'
                />
              </label>
            </>
          )}

          <button
            className="settings-window__secondary"
            onClick={runAiTest}
            disabled={testing !== null}
          >
            {testing === "ai" ? "Testing..." : "Test AI"}
          </button>
        </section>

        <section className="settings-section settings-section--row">
          <div>
            <h2>Launch at login</h2>
          </div>
          <label className="settings-toggle">
            <input
              type="checkbox"
              checked={launchAtLogin}
              onChange={(e) => toggleLaunchAtLogin(e.target.checked)}
            />
            <span />
          </label>
        </section>

        <section className="settings-section settings-section--row">
          <div>
            <h2>Keep popover open when viewing in Discord</h2>
          </div>
          <label className="settings-toggle">
            <input
              type="checkbox"
              checked={settings.keepPopoverOpenOnView}
              onChange={(e) =>
                update("keepPopoverOpenOnView", e.target.checked)
              }
            />
            <span />
          </label>
        </section>
      </main>
    </div>
  );
}
