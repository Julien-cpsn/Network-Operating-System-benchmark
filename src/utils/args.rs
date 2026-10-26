use crate::args::args::ExperimentSelectionArgs;

pub fn experiment_selection_args_to_string(experiment_selection_args: &ExperimentSelectionArgs, runs: u8) -> String {
    let mut str = String::new();

    if let Some(os) = &experiment_selection_args.os {
        str += &format!("--os=\"{}\" ", os);
    }

    if let Some(test_batch) = &experiment_selection_args.test_batch {
        str += &format!("--test-batch=\"{}\" ", test_batch);
    }

    if let Some(resources) = &experiment_selection_args.resources {
        str += &format!("--resources=\"{}\" ", resources.to_command());
    }

    if let Some(topology) = &experiment_selection_args.topology {
        str += &format!("--topology=\"{}\" ", topology);
    }

    if let Some(nic) = &experiment_selection_args.nic {
        str += &format!("--nic=\"{}\" ", nic.to_command());
    }

    if let Some(protocol) = &experiment_selection_args.protocol {
        str += &format!("--protocol=\"{}\" ", protocol);
    }

    if let Some(failures) = &experiment_selection_args.failures {
        str += &format!("--failures=\"{}\" ", failures);
    }

    str += &format!("--runs=\"{runs}\"");

    str
}