// PreToolUse hook (Bash|PowerShell): stops a command that would close or kill Game Manager
// processes other than the agent build, target\agent\debug\app.exe. The user's own window must
// never be closed (CLAUDE.md, "Checking the app from an agent session"). A command that names the
// agent build's folder is let through, and so is gm.ps1's Stop-GmApp, which matches it by path.
let input = "";
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  let command = "";
  try {
    const event = JSON.parse(input);
    command = String((event.tool_input && event.tool_input.command) || "");
  } catch {
    return;
  }
  // Stopping processes, in PowerShell, cmd and Bash.
  const stops =
    /\b(Stop-Process|spps|taskkill|tskill|pkill|killall)\b|\.Kill\s*\(|CloseMainWindow\s*\(|(^|[;&|(\s])kill\s+(-\w+\s+)*[%$\d]/i;
  // Game Manager's processes: app.exe, or a process named "app".
  const app =
    /\bapp\.exe\b|(-(Process)?Name|\/IM|Get-Process|gps|ps)\s+['"]?app\b|Name\s+-(eq|like|match)\s+['"]\*?app\b/i;
  const agentBuild = /target[\\/]+agent|Stop-GmApp/i;
  if (stops.test(command) && app.test(command) && !agentBuild.test(command)) {
    process.stderr.write(
      "Blocked: this command could stop the user's own Game Manager window. Only the agent " +
        "build may be closed: match it by its path (target\\agent\\debug\\app.exe), or use " +
        "Stop-GmApp from .claude/skills/check-app/scripts/gm.ps1.\n",
    );
    process.exit(2);
  }
});
