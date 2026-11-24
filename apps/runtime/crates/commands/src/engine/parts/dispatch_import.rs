{
            let report = respire_app::service::import_json(std::path::Path::new(&file))?;
            let status = if report.orphaned > 0 || report.skipped > report.duplicates {
                OutputStatus::Warn
            } else {
                OutputStatus::Ok
            };
            let mut result = ResultEnvelope::new(
                "import",
                status,
                serde_json::to_value(&report)?,
                vec![
                    OutputItem::new("written", OutputStatus::Ok, report.imported.to_string()),
                    OutputItem::new("duplicates", OutputStatus::Ok, report.duplicates.to_string()),
                    OutputItem::new(
                        "skipped",
                        if report.skipped > report.duplicates {
                            OutputStatus::Warn
                        } else {
                            OutputStatus::Ok
                        },
                        report.skipped.to_string(),
                    ),
                    OutputItem::new(
                        "parent links",
                        OutputStatus::Ok,
                        report.reattached.to_string(),
                    ),
                ],
            );
            if report.orphaned > 0 {
                result.errors.push(format!(
                    "{} records had a parent not in the backup; imported as roots",
                    report.orphaned
                ));
            }
            emit_result(result)?;
}
