import { useEffect, useMemo, useState } from "react";
import {
  getAppSettings,
  getLaunchAtLogin,
  saveAppSettings,
  setLaunchAtLogin,
  testAiSettings,
  testDiscordSettings,
} from "./lib/ipc";
import type { AppSettings, SettingsTestResult } from "./lib/types";

const EMPTY_SETTINGS: AppSettings = {
  discordAppId: "",
  discordBotToken: "",
  discordUserToken: "",
  aiModel: "sonnet",
  aiEffort: "low",
  claudeConfigDir: "",
  acpAgentCommand: "",
};

type SaveState = "idle" | "saving" | "saved" | "error";

export default function SettingsApp() {
  const [settings, setSettings] = useState<AppSettings>(EMPTY_SETTINGS);
  const [launchAtLogin, setLaunchAtLoginState] = useState(false);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [message, setMessage] = useState<string | null>(null);
  const [testResult, setTestResult] = useState<SettingsTestResult | null>(null);
  const [testing, setTesting] = useState<"discord" | "ai" | null>(null);

  useEffect(() => {
    getAppSettings()
      .then(setSettings)
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

  const update = (key: keyof AppSettings, value: string) => {
    setSettings((current) => ({ ...current, [key]: value }));
    setSaveState("idle");
    setMessage(null);
    setTestResult(null);
  };

  const save = () => {
    setSaveState("saving");
    setMessage(null);
    saveAppSettings(settings)
      .then((saved) => {
        setSettings(saved);
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
    setTestResult(null);
    testDiscordSettings(settings)
      .then(setTestResult)
      .catch((error) =>
        setTestResult({ ok: false, message: String(error) }),
      )
      .finally(() => setTesting(null));
  };

  const runAiTest = () => {
    setTesting("ai");
    setTestResult(null);
    testAiSettings(settings)
      .then(setTestResult)
      .catch((error) =>
        setTestResult({ ok: false, message: String(error) }),
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
        setTestResult({ ok: false, message: String(error) });
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
        <section className="settings-section">
          <h2>Discord</h2>
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
          <button
            className="settings-window__secondary"
            onClick={runDiscordTest}
            disabled={testing !== null}
          >
            {testing === "discord" ? "Testing..." : "Test Discord"}
          </button>
        </section>

        <section className="settings-section">
          <h2>AI Extraction</h2>
          <div className="settings-grid">
            <label className="settings-field">
              <span>Model</span>
              <select
                value={settings.aiModel}
                onChange={(e) => update("aiModel", e.target.value)}
              >
                <option value="sonnet">sonnet</option>
                <option value="haiku">haiku</option>
              </select>
            </label>
            <label className="settings-field">
              <span>Effort</span>
              <select
                value={settings.aiEffort}
                onChange={(e) => update("aiEffort", e.target.value)}
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
              value={settings.claudeConfigDir}
              onChange={(e) => update("claudeConfigDir", e.target.value)}
              placeholder="/path/to/claude-config"
            />
          </label>
          <label className="settings-field">
            <span>ACP command</span>
            <input
              value={settings.acpAgentCommand}
              onChange={(e) => update("acpAgentCommand", e.target.value)}
              placeholder="npx -y @agentclientprotocol/claude-agent-acp@0.33.1"
            />
          </label>
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

        {(message || testResult) && (
          <div
            className={
              testResult
                ? testResult.ok
                  ? "settings-message settings-message--ok"
                  : "settings-message settings-message--error"
                : saveState === "error"
                  ? "settings-message settings-message--error"
                  : "settings-message settings-message--ok"
            }
          >
            {testResult ? testResult.message : message}
          </div>
        )}
      </main>
    </div>
  );
}
