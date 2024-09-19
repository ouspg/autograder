use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::config::{BuildConfig, Builder, CourseConfiguration, OutputKind, Task};
use crate::flag_generator::Flag;

const OUTPUT_DIRECTORY: &str = "output/";

fn create_flag_id_pairs_by_task<'a>(
    task_config: &'a Task,
    course_config: &'a CourseConfiguration,
    uuid: Uuid,
) -> (Vec<Flag>, HashMap<String, String>) {
    let mut flags_pairs = HashMap::with_capacity(task_config.stages.len());
    let mut flags = Vec::with_capacity(task_config.stages.len());
    for stage in &task_config.stages {
        // let task_id = part.id.clone();
        let id: &str;
        if let Some(stage_id) = &stage.id {
            id = stage_id;
        } else {
            id = &task_config.id
        }
        match stage.flag.kind.as_str() {
            "user_derived" => {
                let flag = Flag::new_user_flag(
                    id.into(),
                    &course_config.flag_types.user_derived.algorithm,
                    &course_config.flag_types.user_derived.secret,
                    id,
                    &uuid,
                );
                let (flag_key, flag_value) = flag.get_flag_type_value_pair();
                flags_pairs.insert(flag_key, flag_value);
                flags.push(flag);
            }
            "pure_random" => {
                let flag =
                    Flag::new_random_flag(id.into(), course_config.flag_types.pure_random.length);
                let (flag_key, flag_value) = flag.get_flag_type_value_pair();
                flags_pairs.insert(flag_key, flag_value);
                flags.push(flag);
            }
            "rng_seed" => {
                let flag = Flag::new_user_seed_flag(
                    id.into(),
                    &course_config.flag_types.user_derived.algorithm,
                    &course_config.flag_types.user_derived.secret,
                    id,
                    &uuid,
                );
                let (flag_key, flag_value) = flag.get_flag_type_value_pair();
                flags_pairs.insert(flag_key, flag_value);
                flags.push(flag);
            }
            _ => panic!("Invalid flag type"),
        };
    }
    (flags, flags_pairs)
}

#[allow(dead_code)]
fn get_build_info(
    course_config: &mut CourseConfiguration,
    //week_number: u8, needed?
    task_id: String,
) -> Result<BuildConfig, String> {
    for week in &mut course_config.weeks {
        for task in &week.tasks {
            if task_id == task.id {
                return Ok(task.build.clone());
            }
        }
    }
    Err(format!(
        "Build information for task with id {} not found!",
        task_id
    ))
}

/// Build output of the complete single task
/// Single task can have subtasks, which requires embedding multiple flags at once
#[derive(Debug)]
pub struct TaskBuildProcessOutput {
    pub uiid: Uuid,
    pub flags: Vec<Flag>,
    pub files: Vec<OutputKind>,
}
impl TaskBuildProcessOutput {
    pub fn new(uuid: Uuid, flags: Vec<Flag>, files: Vec<OutputKind>) -> Self {
        // The task can have only one file related to instructions
        let readme_count = files
            .iter()
            .filter(|output| matches!(output, OutputKind::Readme(_)))
            .count();
        if readme_count != 1 {
            tracing::error!(
                "The build process output must have exactly one readme file, found {}.",
                readme_count
            );
            std::process::exit(1);
        }
        Self {
            uiid: uuid,
            flags,
            files,
        }
    }
    /// Files that should be delivered for the player or student
    pub fn get_resource_files(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.iter().filter_map(|output| match output {
            OutputKind::Resource(path) => Some(path),
            _ => None,
        })
    }
}

// #[tracing::instrument]
pub fn build_task(
    course_config: &CourseConfiguration,
    task_id: &str,
    uuid: Uuid,
) -> Result<TaskBuildProcessOutput, Box<dyn std::error::Error>> {
    let task_config = course_config.get_task_by_id(task_id).unwrap_or_else(|| {
        tracing::error!(
            "Task with id {} not found in the course configuration. Cannot continue.",
            task_id
        );
        std::process::exit(1);
    });
    let (flags, mut build_envs) = create_flag_id_pairs_by_task(task_config, course_config, uuid);

    match task_config.build.builder {
        Builder::Shell(ref entrypoint) => {
            let build_output = Path::new(OUTPUT_DIRECTORY).join(uuid.to_string());
            let builder_relative_dir = Path::new(&task_config.build.directory).join(&build_output);
            tracing::debug!(
                "Running shell command: {} with flags: {:?} in directory: {}",
                entrypoint.entrypoint,
                build_envs,
                &builder_relative_dir.display()
            );
            // Create all required directories in the path
            match fs::create_dir_all(&builder_relative_dir) {
                Ok(_) => (),
                Err(e) => {
                    tracing::error!(
                        "Failed to create the output directory for task {}: {}. Confirm the task build directory configuration.",
                        task_id,
                        e
                    );
                    std::process::exit(1);
                }
            }
            // The current working directory is set to be the build directory
            // This means that output directory is right after relatively referenced
            build_envs.insert(
                "OUTPUT_DIR".to_string(),
                build_output.to_str().unwrap_or_default().to_string(),
            );
            let output = std::process::Command::new("sh")
                .arg(&entrypoint.entrypoint)
                .env_clear()
                .envs(&build_envs)
                .current_dir(&task_config.build.directory)
                .output();

            let output = match output {
                Ok(output) => output,
                Err(e) => {
                    tracing::error!(
                        "The build process of task {} failed prematurely: {}",
                        task_id,
                        e
                    );
                    std::process::exit(1);
                }
            };
            let mut outputs = Vec::with_capacity(task_config.build.output.len());
            if output.status.success() {
                for output in &task_config.build.output {
                    let path = builder_relative_dir
                        .join(output.kind.get_filename())
                        .canonicalize()?;
                    match fs::metadata(&path) {
                        Ok(_) => {
                            tracing::debug!("File exists: {}", path.display());
                            outputs.push(output.kind.with_new_path(path));
                        }

                        Err(_) => {
                            tracing::error!("File does not exist: {}", path.display());
                            tracing::error!(
                                "The file was configured output with '{}' use case",
                                output.kind.kind()
                            );
                            std::process::exit(1);
                        }
                    }
                }
                Ok(TaskBuildProcessOutput::new(uuid, flags, outputs))
            } else {
                tracing::error!(
                    "The build process for task {} ended with non-zero exit code: {}",
                    task_id,
                    std::str::from_utf8(&output.stderr).unwrap()
                );
                std::process::exit(1);
            }
        }
        Builder::Nix(_) => todo!("Nix builder not implemented"),
    }
}
