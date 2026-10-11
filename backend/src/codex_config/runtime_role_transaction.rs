//! Transaction boundary for refreshing generated subagent roles.
//!
//! The write order and rollback order are part of the runtime compatibility
//! contract: pending policy, role files, lease, then committed policy.

use super::*;

#[derive(Clone, Debug)]
struct RuntimeAgentFileSnapshot {
    path: PathBuf,
    contents: Option<Vec<u8>>,
}

pub(super) fn refresh_runtime_subagent_roles_at(
    config: &CodeyConfig,
    marker: &Path,
    remote_compaction_models: Option<&HashSet<String>>,
) -> Result<()> {
    anyhow::ensure!(
        config.subagent_optimization,
        "当前 Codey 配置未启用子代理协作优化"
    );
    let original_lease = fs::read(marker)
        .with_context(|| format!("读取 Codey Codex lease 失败：{}", marker.display()))?;
    let mut state = serde_json::from_slice::<RuntimeConfigLease>(&original_lease)
        .with_context(|| format!("解析 Codey Codex lease 失败：{}", marker.display()))?;
    anyhow::ensure!(
        state.subagent_optimization_applied,
        "当前 Codex 运行时未注册 Codey 子代理任务类型，需要重启 Codex"
    );

    let constraints_dir = marker.with_file_name(CODEY_CONSTRAINTS_DIR);
    create_private_dir_all(&constraints_dir)?;
    let runtime_roles = runtime_subagent_roles(
        Some(&config.subagent_roles),
        &config.subagent_model,
        &config.subagent_reasoning_effort,
    );
    let registered_roles = registered_roles_for_lease(&state);
    let registration_roles = runtime_registration_roles(
        &runtime_roles,
        &registered_roles,
        &config.subagent_model,
        &config.subagent_reasoning_effort,
    )?;
    let fastctx_instructions = runtime_fastctx_instructions(&constraints_dir, &state)?;
    let plans = plan_runtime_agent_files(
        &constraints_dir,
        &registration_roles,
        fastctx_instructions.as_deref(),
        remote_compaction_models,
    )
    .context("预检 Codey 子代理运行时配置失败；未写入运行时配置")?;
    let expected_hashes = runtime_agent_plan_hashes(&plans);
    let runtime_home = runtime_home_for_lease(&state);
    let mut snapshots = snapshot_runtime_agent_files(&constraints_dir)?;
    let (runtime_policy_path, runtime_policy_pending_path) =
        crate::subagent_gate::runtime_subagent_policy_paths(&runtime_home);
    snapshots.push(RuntimeAgentFileSnapshot {
        contents: read_optional(&runtime_policy_path)?,
        path: runtime_policy_path,
    });
    snapshots.push(RuntimeAgentFileSnapshot {
        contents: read_optional(&runtime_policy_pending_path)?,
        path: runtime_policy_pending_path,
    });

    let update = (|| -> Result<()> {
        crate::subagent_gate::begin_runtime_subagent_policy_update(
            &runtime_home,
            &runtime_roles,
            &expected_hashes,
        )?;
        let registrations = prepare_runtime_agent_files(
            &constraints_dir,
            &registration_roles,
            fastctx_instructions.as_deref(),
            remote_compaction_models,
        )?;
        verify_runtime_agent_files(&registrations, registration_roles.len())?;
        state.subagent_model.clone_from(&config.subagent_model);
        state
            .subagent_reasoning_effort
            .clone_from(&config.subagent_reasoning_effort);
        state.subagent_roles.clone_from(&runtime_roles);
        state
            .registered_subagent_roles
            .clone_from(&registered_roles);
        state.runtime_home.clone_from(&runtime_home);
        state.runtime_agent_schema_version = RUNTIME_AGENT_SCHEMA_VERSION;
        state.runtime_agent_hashes.clone_from(&expected_hashes);
        state.remote_compaction_models = remote_compaction_models.cloned().unwrap_or_default();
        write_lease(marker, &state)?;
        crate::subagent_gate::commit_runtime_subagent_policy(
            &runtime_home,
            &runtime_roles,
            &expected_hashes,
        )
    })();

    if let Err(error) = update {
        if let Err(rollback_error) =
            restore_runtime_agent_files_and_lease(&snapshots, marker, &original_lease)
        {
            anyhow::bail!(
                "更新 Codey 子代理运行时文件失败：{error:#}；回滚运行时文件也失败：{rollback_error:#}"
            );
        }
        return Err(error).context("更新 Codey 子代理运行时文件失败；已恢复原配置");
    }
    Ok(())
}

fn snapshot_runtime_agent_files(constraints_dir: &Path) -> Result<Vec<RuntimeAgentFileSnapshot>> {
    SUBAGENT_ROLE_IDS
        .into_iter()
        .map(|role| {
            let path = runtime_agent_path(constraints_dir, role);
            let contents = read_optional(&path)?;
            Ok(RuntimeAgentFileSnapshot { path, contents })
        })
        .collect()
}

fn verify_runtime_agent_files(
    registrations: &[RuntimeAgentRegistration],
    expected_count: usize,
) -> Result<()> {
    anyhow::ensure!(
        registrations.len() == expected_count,
        "Codey 子代理运行时文件数量不完整"
    );
    for registration in registrations {
        let contents = fs::read(&registration.config_file).with_context(|| {
            format!(
                "读取 Codey 子代理运行时文件失败：{}",
                registration.config_file.display()
            )
        })?;
        anyhow::ensure!(
            crate::fs_util::sha256_hex(&contents) == registration.content_sha256,
            "Codey 子代理运行时文件校验不一致：{}",
            registration.role
        );
    }
    Ok(())
}

fn restore_runtime_agent_files_and_lease(
    snapshots: &[RuntimeAgentFileSnapshot],
    marker: &Path,
    original_lease: &[u8],
) -> Result<()> {
    let mut failures = Vec::new();
    for snapshot in snapshots {
        if let Err(error) = restore_optional_bytes(&snapshot.path, snapshot.contents.as_deref()) {
            failures.push(format!("{}：{error:#}", snapshot.path.display()));
        }
    }
    if let Err(error) = atomic_write(marker, original_lease) {
        failures.push(format!("{}：{error:#}", marker.display()));
    }
    anyhow::ensure!(failures.is_empty(), "{}", failures.join("；"));
    Ok(())
}
