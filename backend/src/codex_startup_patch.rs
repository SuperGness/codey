#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use anyhow::Result;

pub const PATCH_RESULT: &str = "codey-startup-patch-installed-v4";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchOptions {
    pub disable_pet: bool,
    pub disable_voice: bool,
}

pub fn inspector_argument(port: u16) -> String {
    format!("--inspect-brk=127.0.0.1:{port}")
}

const STARTUP_PATCH_TEMPLATE: &str = r#"
(() => {
  const disablePet = __DISABLE_PET__;
  const disableVoice = __DISABLE_VOICE__;
  const disableWindowsOptimizations = process.platform === "win32";
  const disableMicro = disableWindowsOptimizations;
  const disableWindowsWmiSampler = disableWindowsOptimizations;
  const Module = process.getBuiltinModule("module");
  const originalLoad = Module._load;
  const isInspectorArgument = (argument) =>
    typeof argument === "string" && /^--inspect(?:-brk)?(?:=|$)/.test(argument);

  // The inspector is only a startup injection mechanism. Do not pass its
  // pause state or command-line flags to Codex workers.
  process.execArgv.splice(
    0,
    process.execArgv.length,
    ...process.execArgv.filter((argument) => !isInspectorArgument(argument)),
  );
  process.argv.splice(
    0,
    process.argv.length,
    ...process.argv.filter((argument) => !isInspectorArgument(argument)),
  );

  const workerThreads = process.getBuiltinModule("worker_threads");
  const NativeWorker = workerThreads.Worker;
  if (!NativeWorker.__codeyNoInspectWrapper) {
    const EventEmitter = process.getBuiltinModule("events").EventEmitter;
    const isWmiSnapshotWorker = (filename) =>
      disableWindowsWmiSampler &&
      /(?:^|[/\\])child-process-snapshot-worker\.js(?:[?#].*)?$/i.test(String(filename));

    // Codex starts this telemetry worker every 30 seconds. On Windows the
    // worker shells out to PowerShell for two full CIM/WMI process scans.
    // Return the protocol's valid empty snapshot without creating a thread,
    // process, timer, or PowerShell child.
    class CodeyDisabledWmiSnapshotWorker extends EventEmitter {
      constructor() {
        super();
        this.threadId = -1;
        this.stdin = null;
        this.stdout = null;
        this.stderr = null;
        this.codeyTerminated = false;
        process.nextTick(() => {
          if (this.codeyTerminated) return;
          this.emit("message", { type: "ok", value: [] });
          this.emit("exit", 0);
        });
      }
      postMessage() {}
      ref() { return this; }
      unref() { return this; }
      terminate() {
        if (!this.codeyTerminated) {
          this.codeyTerminated = true;
          process.nextTick(() => this.emit("exit", 0));
        }
        return Promise.resolve(0);
      }
    }

    class CodeyNoInspectWorker extends NativeWorker {
      constructor(filename, options = {}) {
        if (isWmiSnapshotWorker(filename)) {
          return new CodeyDisabledWmiSnapshotWorker();
        }
        super(filename, {
          ...options,
          execArgv: options.execArgv ?? [],
        });
      }
    }
    Object.defineProperty(CodeyNoInspectWorker, "__codeyNoInspectWrapper", {
      value: true,
    });
    workerThreads.Worker = CodeyNoInspectWorker;
  }

  const petNoop = () => undefined;
  const petAsyncNoop = async () => undefined;
  const disabledPetManager = new Proxy(
    {
      close: petNoop,
      getCompositionSurfaceHost: () => null,
      getFeedbackLogEntries: () => [],
      getVisibleWebContents: () => null,
      handleRendererReady: petNoop,
      hide: petNoop,
      isOpen: () => false,
      open: petAsyncNoop,
      reconcileRemoteHostedPIPContentHost: petNoop,
      restoreOpenState: petAsyncNoop,
      setFeedbackDiagnosticsEnabled: petNoop,
      toggle: petAsyncNoop,
      wake: petAsyncNoop,
    },
    {
      get(target, property, receiver) {
        return Reflect.has(target, property)
          ? Reflect.get(target, property, receiver)
          : petNoop;
      },
    },
  );
  Object.defineProperty(globalThis, "__CODEY_DISABLED_PET_MANAGER__", {
    configurable: false,
    value: disabledPetManager,
    writable: false,
  });

  const temporaryWebViews = new WeakMap();
  const temporaryWebViewLifecycle = Object.freeze({
    close(owner, partition) {
      const guests = temporaryWebViews.get(owner);
      const guest = guests?.get(partition);
      guests?.delete(partition);
      if (guests?.size === 0) temporaryWebViews.delete(owner);
      if (guest != null && !guest.isDestroyed()) guest.close();
    },
    track(owner, partition, guest) {
      let guests = temporaryWebViews.get(owner);
      if (guests == null) {
        guests = new Map();
        temporaryWebViews.set(owner, guests);
      }
      const previous = guests.get(partition);
      if (previous != null && previous !== guest && !previous.isDestroyed()) previous.close();
      guests.set(partition, guest);
      guest.once("destroyed", () => {
        if (guests.get(partition) === guest) guests.delete(partition);
        if (guests.size === 0) temporaryWebViews.delete(owner);
      });
    },
  });
  Object.defineProperty(globalThis, "__CODEY_TEMP_WEBVIEW_LIFECYCLE__", {
    configurable: false,
    value: temporaryWebViewLifecycle,
    writable: false,
  });

  const installExecutionReaper = ({ connection, kill, snapshot }) => {
    const activeTurns = new Set();
    const idleTimeoutMs = 5 * 60 * 1000;
    let cleanupPromise = null;
    let idleTimer = null;
    let disposed = false;

    const isReclaimable = (processInfo) => {
      const command = String(processInfo?.command ?? "");
      return (
        processInfo?.kind === "mcp" ||
        /(?:^|[/\\])node_repl(?:\.exe)?(?:\s|$)/i.test(command) ||
        /(?:^|[/\\])codegraph\.js\s+serve\b[^\r\n]*--mcp\b/i.test(command) ||
        /(?:^|\s|[/\\])mcp[/\\]server\.mjs(?:\s|$)/i.test(command)
      );
    };

    const reclaim = (reason) => {
      if (disposed || activeTurns.size > 0 || cleanupPromise != null) return cleanupPromise;
      cleanupPromise = Promise.resolve()
        .then(snapshot)
        .then(async (processes) => {
          const candidates = processes
            .filter(isReclaimable)
            .sort((left, right) => (right.depth ?? 0) - (left.depth ?? 0));
          for (const processInfo of candidates) {
            if (disposed || activeTurns.size > 0) break;
            try { await kill(processInfo.pid); } catch {}
          }
          return { reason, reclaimed: candidates.length };
        })
        .catch(() => ({ reason, reclaimed: 0 }))
        .finally(() => { cleanupPromise = null; });
      return cleanupPromise;
    };

    const armIdleTimeout = () => {
      if (idleTimer != null) clearTimeout(idleTimer);
      idleTimer = setTimeout(() => {
        idleTimer = null;
        if (activeTurns.size === 0) void reclaim("idle-timeout");
      }, idleTimeoutMs);
      idleTimer.unref?.();
    };

    const unsubscribe = connection.registerInternalNotificationHandler((notification) => {
      const threadId = notification?.params?.threadId;
      const turnId = notification?.params?.turn?.id;
      if (typeof threadId !== "string" || typeof turnId !== "string") return;
      const key = `${threadId}:${turnId}`;
      if (notification.method === "turn/started") {
        activeTurns.add(key);
        armIdleTimeout();
        return;
      }
      if (notification.method !== "turn/completed") return;
      activeTurns.delete(key);
      armIdleTimeout();
      if (activeTurns.size === 0) {
        const timer = setTimeout(() => void reclaim("task-completed"), 1000);
        timer.unref?.();
      }
    });
    armIdleTimeout();
    return () => {
      disposed = true;
      if (idleTimer != null) clearTimeout(idleTimer);
      activeTurns.clear();
      unsubscribe();
    };
  };
  Object.defineProperty(globalThis, "__CODEY_INSTALL_EXECUTION_REAPER__", {
    configurable: false,
    value: installExecutionReaper,
    writable: false,
  });

  // The pet manager and macOS native composition bridge live inside the
  // monolithic main bundle. Replace the pet manager construction before V8
  // compiles that bundle so the feature owns no timers, windows, native
  // bridge, or lifecycle subscriptions. Voice remains initialized because
  // Codex's settings preload gate awaits responses from its lifecycle
  // manager; the renderer and BrowserWindow guards below disable its UI and
  // resources without deadlocking startup.
  {
    const originalJsExtension = Module._extensions[".js"];
    Module._extensions[".js"] = function codeyMainBundleCompileHook(module, filename) {
      const isCodexMainBundle =
        /[\\/]\.vite[\\/]build[\\/]main-[^\\/]+\.js$/i.test(filename);
      if (!isCodexMainBundle) {
        return Reflect.apply(originalJsExtension, this, arguments);
      }

      const fs = process.getBuiltinModule("fs");
      let source = fs.readFileSync(filename, "utf8");
      if (disablePet) {
      if (
        !source.includes("electron-avatar-overlay-open") ||
        !source.includes("avatar-overlay-composition-surface-preload.js")
      ) {
        throw new Error("Codey pet hard-disable anchors not found in Codex main bundle");
      }
      const managerReference = source.match(
        /getVisibleNativePetWebContents:\(\)=>([$A-Z_a-z][$\w]*)\.getVisibleWebContents\(\)/,
      );
      if (!managerReference) {
        throw new Error("Codey could not identify the Codex pet manager");
      }
      const managerName = managerReference[1];
      const escapedManagerName = managerName.replace(/[$]/g, "\\$&");
      const assignmentPattern = new RegExp(
        "(?:^|[,;])" + escapedManagerName + "=new [$A-Z_a-z][$\\w]*\\(",
      );
      const assignment = assignmentPattern.exec(source);
      if (!assignment) {
        throw new Error("Codey could not locate the Codex pet manager constructor");
      }
      const newOffset = assignment[0].indexOf("new ");
      const valueStart = assignment.index + newOffset;
      const openParen = assignment.index + assignment[0].length - 1;
      let depth = 0;
      let valueEnd = -1;
      for (let index = openParen; index < source.length; index += 1) {
        if (source[index] === "(") depth += 1;
        else if (source[index] === ")" && --depth === 0) {
          valueEnd = index + 1;
          break;
        }
      }
      if (valueEnd < 0) {
        throw new Error("Codey could not bound the Codex pet manager constructor");
      }
      source =
        source.slice(0, valueStart) +
        "globalThis.__CODEY_DISABLED_PET_MANAGER__" +
        source.slice(valueEnd);
      globalThis.__CODEY_PET_MANAGER_SOURCE_REMOVED__ = true;
      }

      const presentationCall = source.match(
        /case`checkout-webview-presentation-changed`:([$A-Z_a-z][$\w]*)\(([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*)\);break/,
      );
      if (!presentationCall) {
        throw new Error("Codey temporary WebView close anchor not found");
      }
      const presentationFunctionName = presentationCall[1].replace(/[$]/g, "\\$&");
      const presentationFunction = new RegExp(
        "function " + presentationFunctionName +
          "\\(([$A-Z_a-z][$\\w]*),\\{partition:([$A-Z_a-z][$\\w]*),url:([$A-Z_a-z][$\\w]*)\\}\\)\\{",
      ).exec(source);
      if (!presentationFunction) {
        throw new Error("Codey temporary WebView presentation handler not found");
      }
      const ownerName = presentationFunction[1];
      const partitionName = presentationFunction[2];
      const urlName = presentationFunction[3];
      const closeBranch = `if(${urlName}==null){`;
      const closeBranchOffset = source.indexOf(closeBranch, presentationFunction.index);
      if (closeBranchOffset < 0 || closeBranchOffset > presentationFunction.index + 1000) {
        throw new Error("Codey temporary WebView close branch not found");
      }
      source =
        source.slice(0, closeBranchOffset + closeBranch.length) +
        `globalThis.__CODEY_TEMP_WEBVIEW_LIFECYCLE__.close(${ownerName},${partitionName});` +
        source.slice(closeBranchOffset + closeBranch.length);

      const attachFunctionPattern =
        /function [$A-Z_a-z][$\w]*\(\{getAuthToken:[$A-Z_a-z][$\w]*[^{}]{0,500},owner:([$A-Z_a-z][$\w]*)\}\)\{/g;
      let attachFunction = null;
      for (const candidate of source.matchAll(attachFunctionPattern)) {
        const nearby = source.slice(candidate.index, candidate.index + 2500);
        if (nearby.includes("will-attach-webview") && nearby.includes("did-attach-webview")) {
          attachFunction = candidate;
          break;
        }
      }
      if (!attachFunction) {
        throw new Error("Codey temporary WebView attach handler not found");
      }
      const attachOwnerName = attachFunction[1];
      const attachTail = source.slice(attachFunction.index, attachFunction.index + 3000);
      const shiftedEntry =
        /let ([$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*\.shift\(\);if\(\1==null\)return;/.exec(attachTail);
      if (!shiftedEntry) {
        throw new Error("Codey temporary WebView attachment queue not found");
      }
      const guestReference = /webContents:([$A-Z_a-z][$\w]*)/.exec(
        attachTail.slice(shiftedEntry.index + shiftedEntry[0].length),
      );
      if (!guestReference) {
        throw new Error("Codey temporary WebView guest reference not found");
      }
      const trackOffset = attachFunction.index + shiftedEntry.index + shiftedEntry[0].length;
      source =
        source.slice(0, trackOffset) +
        `globalThis.__CODEY_TEMP_WEBVIEW_LIFECYCLE__.track(${attachOwnerName},${shiftedEntry[1]}.partition,${guestReference[1]});` +
        source.slice(trackOffset);

      const reaperAnchorPattern =
        /([$A-Z_a-z][$\w]*)\.add\(([$A-Z_a-z][$\w]*)\(\{appServerConnection:([$A-Z_a-z][$\w]*)\(\),closeActiveTurn:([$A-Z_a-z][$\w]*)\.closeActiveTurn\}\)\);/;
      const reaperAnchor = reaperAnchorPattern.exec(source);
      if (!reaperAnchor) {
        throw new Error("Codey execution reaper completion anchor not found");
      }
      const reaperTail = source.slice(reaperAnchor.index, reaperAnchor.index + 5000);
      const processManagerReference =
        /new [$A-Z_a-z][$\w]*\(([$A-Z_a-z][$\w]*)\.getBrowserSessionRegistry\(\)\)/.exec(reaperTail);
      if (!processManagerReference) {
        throw new Error("Codey execution process manager anchor not found");
      }
      const disposerName = reaperAnchor[1];
      const connectionFactoryName = reaperAnchor[3];
      const processManagerName = processManagerReference[1];
      const reaperInstall =
        `${disposerName}.add(globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({` +
        `connection:${connectionFactoryName}(),` +
        `snapshot:()=>${processManagerName}.listProcessManagerSnapshot(),` +
        `kill:async pid=>(await ${processManagerName}.handlers["child-process-kill"]({pid})).killed` +
        `}));`;
      const reaperOffset = reaperAnchor.index + reaperAnchor[0].length;
      source = source.slice(0, reaperOffset) + reaperInstall + source.slice(reaperOffset);

      globalThis.__CODEY_TEMP_WEBVIEW_SOURCE_PATCHED__ = true;
      globalThis.__CODEY_EXECUTION_REAPER_SOURCE_PATCHED__ = true;
      module._compile(source, filename);
    };
  }

  const microStub = {
    __codexMicroDisabledLocal: true,
    ConnectionEventType: {
      CONNECTED: "CONNECTED",
      DISCONNECTED: "DISCONNECTED",
      ERROR: "ERROR",
    },
    DeviceType: { Project2077: "Project2077" },
    OAILightingEffect: { off: 0, breath: 1, solid: 2, snake: 3 },
    WLDeviceDiscovery: class NoCodexMicroDeviceDiscovery {
      findWLDevices() { return []; }
    },
    WLDeviceCommImpl: class NoCodexMicroDeviceComm {
      onConnectionEvent() { return () => {}; }
      async connect() {}
      async disconnect() {}
    },
    RPCApiOAI: class NoCodexMicroApi {
      onHidReceived() { return () => {}; }
      onJoystickMove() { return () => {}; }
      async sendLightingConfig() { return true; }
      async sendThreadsLighting() { return true; }
      async getDeviceStatus() { return {}; }
    },
  };

  let electronProxy = null;
  Module._load = function codeyStartupPatchLoader(request, parent, isMain) {
    if (disableMicro && request === "@worklouder/device-kit-oai") return microStub;
    if (
      disablePet &&
      typeof request === "string" &&
      /(?:^|[/\\])avatar(?:-|_)overlay\.node$/i.test(request)
    ) {
      const error = new Error("Codex pet native module disabled by Codey");
      error.code = "CODEY_PET_DISABLED";
      throw error;
    }

    const loaded = Reflect.apply(originalLoad, this, arguments);
    if (
      (!disablePet && !disableVoice) ||
      request !== "electron" ||
      !loaded?.BrowserWindow
    ) return loaded;
    if (electronProxy) return electronProxy;

    const NativeBrowserWindow = loaded.BrowserWindow;
    const CodeyPetBlockedBrowserWindow = new Proxy(NativeBrowserWindow, {
      construct(target, argumentsList) {
        const options = argumentsList[0] ?? {};
        const title = typeof options.title === "string" ? options.title : "";
        const preload = options.webPreferences?.preload;
        const isPetSurface =
          title.startsWith("Pet Surface ") ||
          (typeof preload === "string" &&
            /avatar-overlay-composition-surface-preload\.js$/i.test(preload));
        const isPetOverlay =
          options.width === 356 &&
          options.height === 320 &&
          options.alwaysOnTop === true &&
          options.transparent === true &&
          options.focusable === false &&
          options.show === false &&
          options.frame === false &&
          options.skipTaskbar === true;
        if (disablePet && (isPetSurface || isPetOverlay)) {
          const error = new Error("Codex pet window disabled by Codey");
          error.code = "CODEY_PET_DISABLED";
          throw error;
        }
        const isVoiceWindow =
          options.appearance === "globalDictation" || /^Dictation$/i.test(title);
        if (disableVoice && isVoiceWindow) {
          const error = new Error("Codex voice window disabled by Codey");
          error.code = "CODEY_VOICE_DISABLED";
          throw error;
        }
        return Reflect.construct(target, argumentsList, target);
      },
      get(target, property) {
        if (property === "__codeyPetBlocked") return disablePet;
        if (property === "__codeyVoiceBlocked") return disableVoice;
        const value = Reflect.get(target, property, target);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    electronProxy = new Proxy(loaded, {
      get(target, property, receiver) {
        return property === "BrowserWindow"
          ? CodeyPetBlockedBrowserWindow
          : Reflect.get(target, property, receiver);
      },
    });
    return electronProxy;
  };
  globalThis.__CODEY_CODEX_STARTUP_PATCH__ = Object.freeze({
    disableWindowsOptimizations,
    disableMicro,
    disablePet,
    disableVoice,
    reclaimExecutionEnvironments: true,
    destroyTemporaryWebViews: true,
    disableWindowsWmiSampler,
  });
  setImmediate(() => {
    try { process.getBuiltinModule("inspector").close(); } catch {}
  });
  return "codey-startup-patch-installed-v4";
})()
"#;

fn patch_expression(options: PatchOptions) -> String {
    STARTUP_PATCH_TEMPLATE
        .replace(
            "__DISABLE_PET__",
            if options.disable_pet { "true" } else { "false" },
        )
        .replace(
            "__DISABLE_VOICE__",
            if options.disable_voice {
                "true"
            } else {
                "false"
            },
        )
}

pub fn reserve_loopback_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

pub async fn install(port: u16, options: PatchOptions) -> Result<()> {
    let websocket_url = wait_for_inspector(port).await?;
    let expression = patch_expression(options);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        install_over_websocket(&websocket_url, &expression),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Codex 启动补丁调试会话超时"))??;
    Ok(())
}

async fn wait_for_inspector(port: u16) -> Result<String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_millis(750))
        .build()?;
    let endpoint = format!("http://127.0.0.1:{port}/json/list");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut last_error = "调试端口尚未响应".to_string();

    while tokio::time::Instant::now() < deadline {
        match client.get(&endpoint).send().await {
            Ok(response) if response.status().is_success() => {
                match response.json::<Vec<serde_json::Value>>().await {
                    Ok(targets) => {
                        if let Some(url) = targets.iter().find_map(|target| {
                            target
                                .get("webSocketDebuggerUrl")
                                .and_then(serde_json::Value::as_str)
                        }) {
                            return Ok(url.to_string());
                        }
                        last_error = "调试端口没有可连接的目标".to_string();
                    }
                    Err(error) => last_error = error.to_string(),
                }
            }
            Ok(response) => last_error = format!("调试端口返回 HTTP {}", response.status()),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    anyhow::bail!("等待 Codex 启动补丁超时：{last_error}")
}

async fn install_over_websocket(websocket_url: &str, expression: &str) -> Result<()> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let (mut socket, _) = tokio_tungstenite::connect_async(websocket_url).await?;
    send_command(&mut socket, 1, "Runtime.enable", serde_json::json!({})).await?;
    send_command(&mut socket, 2, "Debugger.enable", serde_json::json!({})).await?;

    let mut runtime_enabled = false;
    let mut debugger_enabled = false;
    let mut continued = false;
    let mut evaluation_sent = false;

    while let Some(message) = socket.next().await {
        let message = message?;
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
                continue;
            }
            Message::Close(_) => anyhow::bail!("Codex 启动补丁调试连接提前关闭"),
        };
        let payload: serde_json::Value = serde_json::from_str(text.as_ref())?;

        match payload.get("id").and_then(serde_json::Value::as_u64) {
            Some(1) => {
                ensure_protocol_success(&payload, "Runtime.enable")?;
                runtime_enabled = true;
            }
            Some(2) => {
                ensure_protocol_success(&payload, "Debugger.enable")?;
                debugger_enabled = true;
            }
            Some(4) => {
                ensure_protocol_success(&payload, "Debugger.evaluateOnCallFrame")?;
                if let Some(exception) = payload
                    .get("result")
                    .and_then(|result| result.get("exceptionDetails"))
                {
                    anyhow::bail!("Codex 启动补丁执行异常：{exception}");
                }
                let value = payload
                    .pointer("/result/result/value")
                    .and_then(serde_json::Value::as_str);
                if value != Some(PATCH_RESULT) {
                    anyhow::bail!("Codex 启动补丁未返回预期状态");
                }
                send_command(&mut socket, 5, "Debugger.resume", serde_json::json!({})).await?;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let _ = socket.close(None).await;
                return Ok(());
            }
            _ => {}
        }

        if runtime_enabled && debugger_enabled && !continued {
            continued = true;
            send_command(
                &mut socket,
                3,
                "Runtime.runIfWaitingForDebugger",
                serde_json::json!({}),
            )
            .await?;
        }

        if payload.get("method").and_then(serde_json::Value::as_str) == Some("Debugger.paused")
            && !evaluation_sent
        {
            let frame_id = payload
                .pointer("/params/callFrames/0/callFrameId")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("Codex 启动补丁没有收到可用的调用栈"))?;
            evaluation_sent = true;
            send_command(
                &mut socket,
                4,
                "Debugger.evaluateOnCallFrame",
                serde_json::json!({
                    "callFrameId": frame_id,
                    "expression": expression,
                    "returnByValue": true,
                    "silent": false,
                }),
            )
            .await?;
        }
    }

    anyhow::bail!("Codex 启动补丁调试连接未返回执行结果")
}

async fn send_command<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let message = serde_json::json!({
        "id": id,
        "method": method,
        "params": params,
    });
    socket
        .send(Message::Text(message.to_string().into()))
        .await?;
    Ok(())
}

fn ensure_protocol_success(payload: &serde_json::Value, method: &str) -> Result<()> {
    if let Some(error) = payload.get("error") {
        anyhow::bail!("{method} 失败：{error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_is_loopback_only_and_pauses_before_startup() {
        assert_eq!(inspector_argument(19321), "--inspect-brk=127.0.0.1:19321");
    }

    #[test]
    fn patch_result_is_stable_for_launch_status_validation() {
        assert_eq!(PATCH_RESULT, "codey-startup-patch-installed-v4");
    }

    #[test]
    fn patch_expression_can_hard_disable_pet_with_platform_gated_windows_optimizations() {
        let expression = patch_expression(PatchOptions {
            disable_pet: true,
            disable_voice: false,
        });

        assert!(expression.contains("const disablePet = true"));
        assert!(
            expression
                .contains("const disableWindowsOptimizations = process.platform === \"win32\"")
        );
        assert!(expression.contains("const disableMicro = disableWindowsOptimizations"));
        assert!(expression.contains("CodeyPetBlockedBrowserWindow"));
        assert!(expression.contains("avatar-overlay-composition-surface-preload"));
        assert!(expression.contains("avatar(?:-|_)overlay"));
        assert!(expression.contains("__CODEY_DISABLED_PET_MANAGER__"));
        assert!(expression.contains("getVisibleNativePetWebContents"));
        assert!(expression.contains("module._compile(source, filename)"));
    }

    #[test]
    fn windows_lag_patch_only_short_circuits_the_wmi_snapshot_worker() {
        let expression = patch_expression(PatchOptions {
            disable_pet: false,
            disable_voice: false,
        });

        assert!(expression.contains("process.platform === \"win32\""));
        assert!(expression.contains("child-process-snapshot-worker\\.js"));
        assert!(expression.contains("CodeyDisabledWmiSnapshotWorker"));
        assert!(expression.contains("this.emit(\"message\", { type: \"ok\", value: [] })"));
        assert!(expression.contains("super(filename, {"));
    }

    #[test]
    fn voice_slimming_preserves_codex_initialization_services() {
        let expression = patch_expression(PatchOptions {
            disable_pet: false,
            disable_voice: true,
        });

        assert!(expression.contains("const disableVoice = true"));
        assert!(!expression.contains("__CODEY_DISABLED_VOICE_MANAGER__"));
        assert!(!expression.contains("isVoiceHelper"));
        assert!(expression.contains("settings preload gate awaits responses"));
        assert!(expression.contains("options.appearance === \"globalDictation\""));
        assert!(expression.contains("CODEY_VOICE_DISABLED"));
    }

    #[test]
    fn automatic_lifecycle_patch_destroys_webviews_and_reclaims_execution_helpers() {
        let expression = patch_expression(PatchOptions {
            disable_pet: false,
            disable_voice: false,
        });

        assert!(expression.contains("__CODEY_TEMP_WEBVIEW_LIFECYCLE__.close"));
        assert!(expression.contains("__CODEY_TEMP_WEBVIEW_LIFECYCLE__.track"));
        assert!(expression.contains("checkout-webview-presentation-changed"));
        assert!(expression.contains("__CODEY_INSTALL_EXECUTION_REAPER__"));
        assert!(expression.contains("turn/completed"));
        assert!(expression.contains("idleTimeoutMs = 5 * 60 * 1000"));
        assert!(expression.contains("codegraph\\.js\\s+serve"));
        assert!(expression.contains("node_repl"));
        assert!(expression.contains("handlers[\"child-process-kill\"]"));
    }

    #[tokio::test]
    async fn inspector_protocol_installs_stub_before_resuming() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();

            for expected_id in [1_u64, 2] {
                let message = socket.next().await.unwrap().unwrap();
                let Message::Text(text) = message else {
                    panic!("expected inspector command");
                };
                let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
                assert_eq!(command["id"], expected_id);
                socket
                    .send(Message::Text(
                        serde_json::json!({"id": expected_id, "result": {}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected runIfWaitingForDebugger");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["method"], "Runtime.runIfWaitingForDebugger");
            socket
                .send(Message::Text(
                    serde_json::json!({"id": 3, "result": {}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "method": "Debugger.paused",
                        "params": {
                            "callFrames": [{"callFrameId": "frame-1"}]
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected evaluateOnCallFrame");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["method"], "Debugger.evaluateOnCallFrame");
            assert_eq!(command["params"]["callFrameId"], "frame-1");
            let expression = command["params"]["expression"].as_str().unwrap();
            assert!(expression.contains("@worklouder/device-kit-oai"));
            assert!(expression.contains("CodeyPetBlockedBrowserWindow"));
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "id": 4,
                        "result": {
                            "result": {
                                "type": "string",
                                "value": PATCH_RESULT
                            }
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected Debugger.resume");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["method"], "Debugger.resume");
        });

        let expression = patch_expression(PatchOptions {
            disable_pet: true,
            disable_voice: false,
        });
        install_over_websocket(&format!("ws://{address}"), &expression)
            .await
            .unwrap();
        server.await.unwrap();
    }
}
