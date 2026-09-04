import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

async function loadPatchExpression() {
  const template = normalizeLineEndings(await readFile(
    new URL("../backend/src/codex_startup_patch.js", import.meta.url),
    "utf8",
  ));
  assert.ok(template, "startup patch template should be readable");
  return template
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll("__REQUIRE_APP_SERVER_RUNTIME_OVERRIDES__", "false");
}

test("API and ChatGPT auth share model-aware native service-tier controls", async () => {
  const Module = process.getBuiltinModule("module");
  const workerThreads = process.getBuiltinModule("worker_threads");
  const nativeLoad = Module._load;
  const nativeJsExtension = Module._extensions[".js"];
  const NativeWorker = workerThreads.Worker;
  let appProtocolHandler = null;

  class FakeBrowserWindow {}
  const fakeElectron = {
    BrowserWindow: FakeBrowserWindow,
    protocol: {
      handle(scheme, handler) {
        assert.equal(scheme, "app");
        appProtocolHandler = handler;
      },
    },
  };
  Module._load = function nativeControlsTestLoader(request) {
    if (request === "electron") return fakeElectron;
    return Reflect.apply(nativeLoad, this, arguments);
  };

  try {
    assert.equal(
      (0, eval)(await loadPatchExpression()),
      "codey-startup-patch-installed-v38",
    );
    Module._load("electron", undefined, false).protocol.handle(
      "app",
      async (request) =>
        request.response ??
        new Response(request.fixture, {
          headers: { "content-type": "text/javascript" },
        }),
    );

    const patchAsset = async (
      fixture,
      url = "app://-/assets/app-initial~native-controls-fixture.js",
    ) => {
      const response = await appProtocolHandler({
        fixture,
        url,
      });
      return response.text();
    };

    const modelSource = [
      "function Ue({authMethod:e,includeUltraReasoningEffort:i,useHiddenModels:o}){",
      "let s=[],c=null,l=o&&e!==`amazonBedrock`,u=i;",
      "return {gate:l,models:s,defaultModel:c,useHiddenModels:o}}",
    ].join("");
    const patchedModel = await patchAsset(modelSource);
    assert.match(patchedModel, /l=o&&e=== `chatgpt`/);
    assert.doesNotMatch(patchedModel, /!==`amazonBedrock`/);
    const modelGate = Function(
      `${patchedModel};return (authMethod) => ` +
        "Ue({authMethod,includeUltraReasoningEffort:true,useHiddenModels:true}).gate;",
    )();
    assert.equal(modelGate("chatgpt"), true);
    assert.equal(modelGate("apikey"), false);

    const nestedAuthModelSource = [
      "function Ue({authMethod:e,includeUltraReasoningEffort:i,useHiddenModels:o}){",
      "let s=[],c=null,l=(o)&&e!==\"amazonBedrock\",u=i;",
      "return {gate:l,models:s,defaultModel:c,useHiddenModels:o}}",
    ].join("");
    const patchedNestedAuthModel = await patchAsset(nestedAuthModelSource);
    assert.match(patchedNestedAuthModel, /l=\(o\)&&e=== `chatgpt`/);
    assert.doesNotMatch(patchedNestedAuthModel, /amazonBedrock/);

    const modelListFilterSource = [
      "function filter({authMethod:e,availableModels:n,includeUltraReasoningEffort:a,models:o,useHiddenModels:s}){",
      "let c=[],l=null,u=s&&e!==`amazonBedrock`;",
      "o.forEach(r=>{if(u?n.has(r.model):!r.hidden){c.push(r.model);r.isDefault&&(l=r.model)}});",
      "return {models:c,defaultModel:l,includeUltraReasoningEffort:a}}",
    ].join("");
    const patchedModelListFilter = await patchAsset(
      modelListFilterSource,
      "app://-/assets/model-list-filter-lLUu6272.js",
    );
    assert.match(
      patchedModelListFilter,
      /if\(u\?\(n\.has\(r\.model\)\|\|!r\.hidden\):!r\.hidden\)/,
    );
    assert.match(patchedModelListFilter, /u=s&&e=== `chatgpt`/);
    const filterModels = Function(
      `${patchedModelListFilter};return filter;`,
    )();
    const models = [
      { hidden: false, isDefault: true, model: "gpt-5.6-sol" },
      { hidden: false, isDefault: false, model: "gpt-5.3-codex-spark" },
      { hidden: true, isDefault: false, model: "hidden-preview" },
    ];
    assert.deepEqual(
      filterModels({
        authMethod: "chatgpt",
        availableModels: new Set(["gpt-5.6-sol", "hidden-preview"]),
        includeUltraReasoningEffort: true,
        models,
        useHiddenModels: true,
      }).models,
      ["gpt-5.6-sol", "gpt-5.3-codex-spark", "hidden-preview"],
    );
    assert.deepEqual(
      filterModels({
        authMethod: "apikey",
        availableModels: new Set(["gpt-5.6-sol"]),
        includeUltraReasoningEffort: true,
        models,
        useHiddenModels: true,
      }).models,
      ["gpt-5.6-sol", "gpt-5.3-codex-spark"],
    );
    assert.deepEqual(
      filterModels({
        authMethod: "chatgpt",
        availableModels: new Set(["hidden-preview"]),
        includeUltraReasoningEffort: true,
        models,
        useHiddenModels: false,
      }).models,
      ["gpt-5.6-sol", "gpt-5.3-codex-spark"],
    );

    const strictHiddenModelListFilterSource = [
      "function filterStrict({authMethod:e,availableModels:n,",
      "includeUltraReasoningEffort:a,models:o,useHiddenModels:s}){",
      "let c=[],u=(s)&&e!==\"amazonBedrock\";",
      "o.forEach(r=>{if((u?n.has(r.model):r.hidden!==!0)){c.push(r.model)}});",
      "return {models:c,includeUltraReasoningEffort:a}}",
    ].join("");
    const patchedStrictHiddenModelListFilter = await patchAsset(
      strictHiddenModelListFilterSource,
      "app://-/assets/model-list-filter-strict-hidden.js",
    );
    assert.match(
      patchedStrictHiddenModelListFilter,
      /if\(u\?\(n\.has\(r\.model\)\|\|!r\.hidden\):!r\.hidden\)/,
    );
    assert.match(patchedStrictHiddenModelListFilter, /u=\(s\)&&e=== `chatgpt`/);

    const consolidatedModelListFilterSource = [
      "function filterV2({additionalAvailableModels:e,authMethod:t,availableModels:n,",
      "includeUltraReasoningEffort:a,models:o,useHiddenModels:s}){",
      "let c=[],u=s&&t!==`amazonBedrock`;",
      "o.forEach(r=>{if(e?.has(r.model)===!0||(u?n.has(r.model):!r.hidden)){c.push(r.model)}});",
      "return {models:c,includeUltraReasoningEffort:a}}",
    ].join("");
    const patchedConsolidatedModelListFilter = await patchAsset(
      consolidatedModelListFilterSource,
      "app://-/assets/app-initial-BTphDPeq.js",
    );
    assert.match(
      patchedConsolidatedModelListFilter,
      /if\(u\?\(n\.has\(r\.model\)\|\|!r\.hidden\):!r\.hidden\)/,
    );
    assert.doesNotMatch(
      patchedConsolidatedModelListFilter,
      /e\?\.has\(r\.model\)===!0\|\|/,
    );
    const filterModelsV2 = Function(
      `${patchedConsolidatedModelListFilter};return filterV2;`,
    )();
    assert.deepEqual(
      filterModelsV2({
        additionalAvailableModels: new Set(["gpt-5.3-codex-spark"]),
        authMethod: "apikey",
        availableModels: new Set(["gpt-5.6-sol"]),
        includeUltraReasoningEffort: true,
        models,
        useHiddenModels: true,
      }).models,
      ["gpt-5.6-sol", "gpt-5.3-codex-spark"],
    );

    const serviceTierUiSource = [
      "function U(e){let o=e,s=o?.authMethod===`chatgpt`,c=o?.authMethod??null,l;",
      "let u=o,f=false,p=s&&!f&&u!=null&&",
      "u?.requirements?.featureRequirements?.fast_mode!==!1,m;",
      "return {authMethod:c,isServiceTierAllowed:p}}",
    ].join("");
    const patchedServiceTierUi = await patchAsset(
      serviceTierUiSource,
      "app://-/assets/use-service-tier-settings-XUBE8MwV.js",
    );
    assert.match(
      patchedServiceTierUi,
      /p=!0/,
    );
    assert.doesNotMatch(
      patchedServiceTierUi,
      /featureRequirements\?\.fast_mode/,
    );
    const serviceTierAllowed = Function(
      `${patchedServiceTierUi};return U;`,
    )();
    assert.equal(
      serviceTierAllowed({
        authMethod: "chatgpt",
        requirements: { featureRequirements: { fast_mode: false } },
      }).isServiceTierAllowed,
      true,
    );
    assert.equal(
      serviceTierAllowed({
        authMethod: "apikey",
        requirements: { featureRequirements: { fast_mode: false } },
      }).isServiceTierAllowed,
      true,
    );

    const serviceTierOptionsSource = [
      "const serviceTierMessageIds=[`serviceTier.standard.label`,`serviceTier.fast.label`];",
      "const messages={fastDescription:`Fast response`,fastLabel:`Fast`};",
      "const standard={description:`Default speed`,iconKind:null,label:`Standard`,tier:null,value:null};",
      "function kind(e){return e===`priority`?`fast`:null}",
      "function description(e){return e.description??messages.fastDescription}",
      "function label(e){return e.id===`priority`?messages.fastLabel:e.name}",
      "function options(e){return[standard,...(e?.serviceTiers??[]).map(e=>({",
      "description:description(e),iconKind:kind(e.id),label:label(e),tier:e,value:e.id}))]}",
      "function lookup(e,t){return e?.serviceTiers?.find(e=>e.id===t)??null}",
      "function selected(e,t){return lookup(e,t)?.id??null}",
    ].join("");
    const patchedServiceTierOptions = await patchAsset(serviceTierOptionsSource);
    assert.equal(patchedServiceTierOptions, serviceTierOptionsSource);
    assert.doesNotMatch(
      patchedServiceTierOptions,
      /serviceTiers\?\.length\?.*priority/,
    );
    const nativeServiceTierHelpers = Function(
      `${patchedServiceTierOptions};return {options,selected};`,
    )();
    assert.deepEqual(
      nativeServiceTierHelpers.options({}).map(({ iconKind, label, value }) => ({
        iconKind,
        label,
        value,
      })),
      [
        { iconKind: null, label: "Standard", value: null },
      ],
    );
    assert.deepEqual(
      nativeServiceTierHelpers
        .options({ serviceTiers: [] })
        .map(({ label, value }) => ({ label, value })),
      [
        { label: "Standard", value: null },
      ],
    );
    assert.equal(nativeServiceTierHelpers.selected({}, "priority"), null);
    const fastServiceTier = {
      description: "1.5x speed",
      id: "priority",
      name: "Fast",
    };
    assert.deepEqual(
      nativeServiceTierHelpers
        .options({ serviceTiers: [fastServiceTier] })
        .map(({ iconKind, label, value }) => ({ iconKind, label, value })),
      [
        { iconKind: null, label: "Standard", value: null },
        { iconKind: "fast", label: "Fast", value: "priority" },
      ],
    );
    const speedControlVisible = (authMethod, serviceTiers) =>
      serviceTierAllowed({
        authMethod,
        requirements: { featureRequirements: { fast_mode: false } },
      }).isServiceTierAllowed &&
      nativeServiceTierHelpers.options({ serviceTiers }).length > 1;
    assert.equal(speedControlVisible("chatgpt", [fastServiceTier]), true);
    assert.equal(speedControlVisible("apikey", [fastServiceTier]), true);
    assert.equal(speedControlVisible("chatgpt", []), false);
    assert.equal(speedControlVisible("apikey", []), false);
    assert.deepEqual(
      nativeServiceTierHelpers
        .options({
          serviceTiers: [
            { description: "Lowest latency", id: "ultrafast", name: "Ultrafast" },
          ],
        })
        .map(({ label, value }) => ({ label, value })),
      [
        { label: "Standard", value: null },
        { label: "Ultrafast", value: "ultrafast" },
      ],
    );

    const serviceTierSettingsUiSource = [
      "function Settings(e){let {isServiceTierAllowed:n}=e,",
      "r=e.serviceTierSettings,{selectedServiceTier:s}=r;",
      "if(!n||r.availableOptions.length<=1)return null;",
      "return {availableOptions:r.availableOptions,selectedServiceTier:s}}",
    ].join("");
    const patchedServiceTierSettingsUi = await patchAsset(
      serviceTierSettingsUiSource,
      "app://-/assets/general-settings-BWZCvLqI.js",
    );
    assert.match(
      patchedServiceTierSettingsUi,
      /if\(r\.availableOptions\.length<=1\)return null/,
    );
    assert.doesNotMatch(patchedServiceTierSettingsUi, /if\(!n\|\|/);
    const nativeSettings = Function(
      `${patchedServiceTierSettingsUi};return Settings;`,
    )();
    assert.deepEqual(
      nativeSettings({
        isServiceTierAllowed: false,
        serviceTierSettings: {
          availableOptions: [
            { label: "Standard", value: null },
            { label: "Fast", value: "priority" },
          ],
          selectedServiceTier: "priority",
        },
      }).availableOptions,
      [
        { label: "Standard", value: null },
        { label: "Fast", value: "priority" },
      ],
    );
    assert.equal(
      nativeSettings({
        isServiceTierAllowed: true,
        serviceTierSettings: {
          availableOptions: [{ label: "Standard", value: null }],
          selectedServiceTier: null,
        },
      }),
      null,
    );

    const modelAwareServiceTierRuntimeSource = [
      "const runtimeMarkers=[`isServiceTierAllowed`,`serviceTierForRequest:`,`availableOptions:`];",
      "function resolveTier(existing,tier,isAllowed,resolver,normalize,model){",
      "let request,selected;",
      "request=existing?isAllowed?tier:null:resolver(model,tier,isAllowed),",
      "selected=request==null?null:normalize(model,request);",
      "let label=labelTier(request??null);",
      "return {availableOptions:[],label,selectedServiceTier:selected,serviceTierForRequest:request}}",
      "function resolveConfiguredTier(model,tier,isAllowed=true){",
      "if(!isAllowed)return null;",
      "return tier==null?model.defaultServiceTier??null:tier}",
      "function normalizeTier(model,tier){",
      "return model.serviceTiers.includes(tier)?tier:null}",
      "function labelTier(tier){return tier??`standard`}",
      "function serviceTierLoading(modelSettings,catalogLoading,configState,conversation,requirementsPending){",
      "let loading=modelSettings.isLoading||catalogLoading||configState.isLoading||",
      "conversation==null&&requirementsPending,done=true;",
      "return {done,loading}}",
      "function composer(isAllowed,settings,isLoading,register){",
      "let fastOption=settings.availableOptions.find(e=>e.iconKind===`fast`)?.value,",
      "show=isAllowed&&settings.availableOptions.length>1;",
      "register(`composer.toggleFastMode`,()=>{},",
      "{enabled:isAllowed&&!isLoading&&fastOption!=null});",
      "return {fastOption,show}}",
      "function speedCommand(isAllowed,settings){",
      "const marker=`composer.speedSlashCommand.disableDescription`;",
      "return settings.availableOptions.map(e=>({",
      "enabled:isAllowed&&!settings.isLoading,isSelected:false,marker,option:e}))}",
    ].join("");
    const patchedModelAwareServiceTierRuntime = await patchAsset(
      modelAwareServiceTierRuntimeSource,
    );
    assert.doesNotMatch(
      patchedModelAwareServiceTierRuntime,
      /show=isAllowed&&/,
    );
    assert.doesNotMatch(
      patchedModelAwareServiceTierRuntime,
      /enabled:isAllowed&&!isLoading/,
    );
    assert.doesNotMatch(
      patchedModelAwareServiceTierRuntime,
      /enabled:isAllowed&&!settings\.isLoading/,
    );
    const modelAwareRuntime = Function(
      `${patchedModelAwareServiceTierRuntime};` +
        "return {composer,normalizeTier,resolveConfiguredTier,resolveTier,serviceTierLoading,speedCommand};",
    )();
    const supportedModel = { serviceTiers: ["priority"] };
    const unsupportedModel = { serviceTiers: [] };
    assert.equal(
      modelAwareRuntime.resolveTier(
        false,
        "priority",
        false,
        modelAwareRuntime.resolveConfiguredTier,
        modelAwareRuntime.normalizeTier,
        supportedModel,
      ).serviceTierForRequest,
      "priority",
    );
    assert.equal(
      modelAwareRuntime.resolveTier(
        false,
        "priority",
        false,
        modelAwareRuntime.resolveConfiguredTier,
        modelAwareRuntime.normalizeTier,
        unsupportedModel,
      ).serviceTierForRequest,
      null,
    );
    assert.equal(
      modelAwareRuntime.serviceTierLoading(
        { isLoading: false },
        false,
        { isLoading: false },
        null,
        true,
      ).loading,
      false,
    );
    assert.equal(
      modelAwareRuntime.serviceTierLoading(
        { isLoading: true },
        false,
        { isLoading: false },
        null,
        false,
      ).loading,
      true,
    );
    assert.equal(
      modelAwareRuntime.resolveTier(
        true,
        "priority",
        false,
        modelAwareRuntime.resolveConfiguredTier,
        modelAwareRuntime.normalizeTier,
        unsupportedModel,
      ).serviceTierForRequest,
      null,
    );
    const registeredCommands = [];
    const supportedControls = modelAwareRuntime.composer(
      false,
      {
        availableOptions: [
          { iconKind: null, value: null },
          { iconKind: "fast", value: "priority" },
        ],
      },
      false,
      (_name, _handler, options) => registeredCommands.push(options),
    );
    assert.equal(supportedControls.show, true);
    assert.equal(registeredCommands[0].enabled, true);
    assert.equal(
      modelAwareRuntime.composer(
        true,
        { availableOptions: [{ iconKind: null, value: null }] },
        false,
        () => {},
      ).show,
      false,
    );
    assert.equal(
      modelAwareRuntime.speedCommand(
        false,
        {
          availableOptions: [{ iconKind: "fast", value: "priority" }],
          isLoading: false,
        },
      )[0].enabled,
      true,
    );

    const serviceTierSanitizerSource = [
      "async function sanitize(e,t){if(e==null)return null;",
      "try{if((await t()).requirements?.featureRequirements?.fast_mode===!1)",
      "return null}catch(e){console.warn(`Failed to load config requirements for service tier`)}",
      "return e}",
    ].join("");
    const patchedServiceTierSanitizer = await patchAsset(
      serviceTierSanitizerSource,
    );
    assert.doesNotMatch(
      patchedServiceTierSanitizer,
      /featureRequirements\?\.fast_mode/,
    );
    const sanitizeServiceTier = Function(
      `${patchedServiceTierSanitizer};return sanitize;`,
    )();
    let entitlementReads = 0;
    assert.equal(
      await sanitizeServiceTier("priority", async () => {
        entitlementReads += 1;
        return {
          requirements: { featureRequirements: { fast_mode: false } },
        };
      }),
      "priority",
    );
    assert.equal(entitlementReads, 0);

    const serviceTierRequestSource = [
      "async function Qs(e,t){let n=await Js(e,t);",
      "if(n!==`chatgpt`)return!1;",
      "let r=await rt(t);return r.requirements?.featureRequirements?.fast_mode!==!1}",
      "function Zs(){throw Error(`Failed to read service tier for request`)}",
    ].join("");
    const patchedServiceTierRequest = await patchAsset(
      serviceTierRequestSource,
      "app://-/assets/read-service-tier-for-request-BJ4fBpQe.js",
    );
    assert.match(
      patchedServiceTierRequest,
      /async function Qs\(e,t\)\{return!0\}/,
    );
    assert.doesNotMatch(
      patchedServiceTierRequest,
      /featureRequirements\?\.fast_mode/,
    );
    const serviceTierRequestAllowed = Function(
      "Js",
      "rt",
      `${patchedServiceTierRequest};return Qs;`,
    )(
      async () => {
        throw new Error("auth lookup must not run");
      },
      async () => {
        throw new Error("entitlement lookup must not run");
      },
    );
    assert.equal(await serviceTierRequestAllowed({}, "host"), true);

    // Third-party catalogs may not meet Codex's native power-selection threshold.
    // Codey still uses the modern trigger and preserves Codex's native Fast
    // indicators for models that expose the selected speed tier.
    const fastModelPresentationSource = [
      "const nativeModelPickerMarkers=[",
      "`composer.intelligenceDropdown.model.title`,",
      "`composer.intelligenceDropdown.model.rowLabel`];",
      "function triggerConfig(hideLabel,powerSelections){",
      "let workMode=true,compact=workMode&&powerSelections.length>=4,",
      "configEnabled=compact&&!hideLabel,focusTarget=compact?`simple`:`advanced`;",
      "return {focusTarget,modelPickerTriggerConfig:configEnabled?",
      "{showFastServiceTierIndicator:true}:void 0}}",
      "function unrelatedFastShapes(workMode,hideDecoy,compact,otherConfig){",
      "let decoyEnabled=workMode&&!hideDecoy;",
      "if(compact&&otherConfig!=null)return {decoyEnabled};return null}",
      "function unrelatedIconShape(decoyIcon,model,tier){",
      "let decoyResult=decoyIcon!=null&&supports(model,tier)?decoyIcon:null;",
      "return decoyResult}",
      "function nativePicker(input,powerSelections,selectedServiceTierIconKind){",
      "let {modelPickerTriggerConfig:config}=input,workMode=true,",
      "compact=workMode&&powerSelections.length>=4,useCompact=compact,",
      "selectedModel={},selectedServiceTier=`priority`,otherConfig=null;",
      "let selectedIcon=!useCompact&&selectedServiceTierIconKind!=null&&",
      "supports(selectedModel,selectedServiceTier)?selectedServiceTierIconKind:null;",
      "let rowIcon=selectedServiceTierIconKind!==null&&supports(selectedModel,selectedServiceTier)?selectedServiceTierIconKind:null;",
      "let labels={selected:{serviceTierIconKind:selectedIcon},row:{serviceTierIconKind:rowIcon}};",
      "let options=[`gpt-5.5`,`claude-opus-4-8`].map(model=>({",
      "model,selectedServiceTierIconKind:useCompact?null:selectedServiceTierIconKind,stripGptPrefix:useCompact}));",
      "if(compact&&otherConfig!=null)return {kind:`decoy`};",
      "if(compact&&config!=null)return {kind:`solid`,labels,options,rowIcon,selectedIcon};",
      "return {kind:`outline`,labels,options,rowIcon,selectedIcon}}",
      "function supports(){return true}",
    ].join("");
    const patchedFastModelPresentation = await patchAsset(
      fastModelPresentationSource,
      "app://-/assets/codex-composer-adapter-DDUHejoe.js",
    );
    assert.match(patchedFastModelPresentation, /configEnabled=!hideLabel/);
    assert.doesNotMatch(patchedFastModelPresentation, /selectedIcon=null/);
    assert.doesNotMatch(patchedFastModelPresentation, /rowIcon=null/);
    assert.doesNotMatch(
      patchedFastModelPresentation,
      /selectedServiceTierIconKind:null,stripGptPrefix:/,
    );
    assert.match(patchedFastModelPresentation, /if\(config!=null\)/);
    assert.match(
      patchedFastModelPresentation,
      /if\(compact&&otherConfig!=null\)/,
    );
    assert.match(
      patchedFastModelPresentation,
      /decoyResult=decoyIcon!=null&&supports\(model,tier\)\?decoyIcon:null/,
    );
    const nativeModelPicker = Function(
      `${patchedFastModelPresentation};return {nativePicker,triggerConfig};`,
    )();
    const thirdPartyTrigger = nativeModelPicker.triggerConfig(false, []);
    const thirdPartyPresentation = nativeModelPicker.nativePicker(
      {
        modelPickerTriggerConfig:
          thirdPartyTrigger.modelPickerTriggerConfig,
      },
      [],
      "fast",
    );
    assert.ok(thirdPartyTrigger.modelPickerTriggerConfig);
    assert.equal(thirdPartyPresentation.kind, "solid");
    assert.equal(thirdPartyPresentation.rowIcon, "fast");
    assert.equal(thirdPartyPresentation.selectedIcon, "fast");
    assert.ok(thirdPartyPresentation.options.every(
      (option) => option.selectedServiceTierIconKind === "fast",
    ));
    assert.equal(
      nativeModelPicker.triggerConfig(true, []).modelPickerTriggerConfig,
      undefined,
    );

    for (const url of [
      "app://-/assets/app-initial.js",
      "app://-/assets/app-initial-windows.js",
      "app://-/assets/app-initial~windows.js?build=store",
      "app://-/assets/codex-composer-adapter-DDUHejoe.js",
      "app://-/assets/general-settings-BWZCvLqI.js",
      "app://-/assets/model-list-filter-lLUu6272.js",
      "app://-/assets/windows-model-controls-a1b2c3.js",
      "app://-/assets/use-service-tier-settings-XUBE8MwV.js",
      "app://-/assets/read-service-tier-for-request-BJ4fBpQe.js",
    ]) {
      assert.match(
        await patchAsset(serviceTierRequestSource, url),
        /async function Qs\(e,t\)\{return!0\}/,
      );
    }
    assert.equal(
      await patchAsset(
        "const unrelatedWindowsChunk = true;",
        "app://-/assets/unrelated-windows-chunk.js",
      ),
      "const unrelatedWindowsChunk = true;",
    );
    const unrelatedResponse = new Response(
      "const unrelatedWindowsChunk = true;",
      { headers: { "content-type": "text/javascript" } },
    );
    Object.defineProperty(unrelatedResponse, "clone", {
      value() {
        throw new Error("unrelated renderer assets must not be cloned");
      },
    });
    const bypassedResponse = await appProtocolHandler({
      response: unrelatedResponse,
      url: "app://-/assets/unrelated-windows-chunk.js",
    });
    assert.equal(bypassedResponse, unrelatedResponse);
  } finally {
    workerThreads.Worker = NativeWorker;
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
  }
});

test("starting or restarting Codex replaces the old runtime with one managed by Codey", async () => {
  const [runtimeSource, launcherSource, launcherProcessSource, launcherPlatformSource, appSource] =
    await Promise.all([
    readFile(new URL("../backend/src/commands/runtime.rs", import.meta.url), "utf8")
      .then(normalizeLineEndings),
    readFile(new URL("../backend/src/launcher.rs", import.meta.url), "utf8")
      .then(normalizeLineEndings),
    readFile(
      new URL("../backend/src/launcher/process.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
    readFile(
      new URL("../backend/src/launcher/platform.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  ]);
  const launcherModules =
    `${launcherSource}\n${launcherProcessSource}\n${launcherPlatformSource}`;
  const restartFlow = runtimeSource.slice(
    runtimeSource.indexOf("pub async fn schedule_restart_codey_runtime"),
    runtimeSource.indexOf("pub async fn stop_codey_runtime"),
  );
  const prepareLaunchFlow = launcherProcessSource.slice(
    launcherProcessSource.indexOf("async fn prepare_codex_for_launch"),
    launcherProcessSource.indexOf("fn startup_patch_detail"),
  );

  assert.match(restartFlow, /runtime_operation\.lock\(\)/);
  assert.match(restartFlow, /stop_codey_runtime_locked\(&restart_state\)/);
  assert.match(restartFlow, /launch_codey_inner_locked\(&restart_state\)/);
  assert.match(restartFlow, /runtime_generation/);
  assert.match(restartFlow, /restart_task/);
  assert.match(restartFlow, /oneshot::channel\(\)/);
  assert.match(restartFlow, /is_shutting_down\(\)/);
  assert.match(restartFlow, /request_shutdown\(\)/);
  assert.match(
    runtimeSource,
    /pub async fn begin_shutdown[\s\S]*?cancel\.send\(\(\)\)[\s\S]*?task\.await/,
  );
  assert.match(
    runtimeSource,
    /runtime_generation\.load\(Ordering::Acquire\) == runtime_generation/,
  );
  assert.match(
    launcherModules,
    /stop_macos_codex\([\s\S]*?inspector_argument,[\s\S]*?&self\.codex_app_path,[\s\S]*?self\.process_id,[\s\S]*?self\.process_group_id/,
  );
  assert.match(
    prepareLaunchFlow,
    /tokio::task::spawn_blocking[\s\S]*?if already_running \{[\s\S]*?terminate_windows_codex_processes\(&app_dir, None\)[\s\S]*?\.await/,
  );
  assert.match(
    prepareLaunchFlow,
    /if macos_codex_is_running\(app_dir\)\.await\? \{[\s\S]*?terminate_unix_codex_processes\(app_dir, None, None, None\)[\s\S]*?\.await/,
  );
  assert.doesNotMatch(prepareLaunchFlow, /anyhow::bail!/);
  assert.match(launcherModules, /build_codex_executable\(app_dir\)/);
  assert.match(launcherModules, /owned_unix_codex_process_ids/);
  assert.match(launcherModules, /libc::SIGKILL/);
  assert.doesNotMatch(runtimeSource, /"close_codex"/);
  assert.doesNotMatch(runtimeSource, /show_manual_relaunch_prompt/);
  assert.match(appSource, /await invoke\("restart_codey"\)/);
  assert.match(appSource, /Codey 将自动重新拉起客户端/);
  assert.doesNotMatch(appSource, /关闭 Codex/);
});
