import { Profiler, useLayoutEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { AgentAvatar } from "../../../src/components/AgentAvatar";
import { diceBearAvatarCache } from "../../../src/avatar-rendering";
import { formatTime } from "../../../src/ui-utils";
import "../../../src/styles.css";

declare global {
  interface Window { __avatarProbe: any; __renderStats: Record<string, number>; }
}
const snapshots: unknown[] = [];
const commits: unknown[] = [];
let resolveLate: (uri: string) => void;
const late = new Promise<string>((resolve) => { resolveLate = resolve; });
const load = diceBearAvatarCache.load;
diceBearAvatarCache.load = (spec) => spec.seed === "late" ? late : load(spec);

function Fixture() {
  const [avatar, setAvatar] = useState("dicebear:dylan:shared");
  const [key, setKey] = useState(0);
  const [nonce, setNonce] = useState(0);
  useLayoutEffect(() => {
    snapshots.push({ key, avatar, blanks: document.querySelectorAll(".stage .agent-avatar-pixels").length });
    window.__avatarProbe = {
      snapshots, commits, rerender: () => setNonce((value) => value + 1),
      remount: () => setKey((value) => value + 1), change: setAvatar,
      resolveLate: () => resolveLate("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg'/%3E"),
    };
  });
  return <Profiler id="avatars" onRender={(_id, phase, actualDuration) => commits.push({ phase, actualDuration })}>
    <div className="stage" key={key} style={{ height: 240, overflow: "auto", padding: 20 }}>
      {Array.from({ length: 30 }, (_, index) => <div key={index} style={{ height: 48 }}>
        <AgentAvatar agent={{ id: "shared", handle: "test", display_name: "Test", status: "idle", runtime: "codex", avatar, subscription_status: null, description: String(nonce) }} />
        <span>{formatTime("2025-01-01T00:00:00Z")}</span>
      </div>)}
    </div>
  </Profiler>;
}
createRoot(document.getElementById("root")!).render(<Fixture />);
