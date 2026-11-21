//! .NET: solutions and projects (`.sln`, `.csproj`/`.fsproj`/`.vbproj`).

use crate::model::TaskKind;
use crate::scan::{quote_arg, Scan, SKIP_DIRS};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::path::{Path, PathBuf};

/// Projects inspected at most.
const MAX_PROJECTS: usize = 40;

#[derive(Debug, Default)]
struct ProjectInfo {
    exe: bool,
    web: bool,
    test: bool,
    ef_design: bool,
}

pub(crate) fn detect(s: &mut Scan<'_>) {
    let root = s.root.to_path_buf();
    let entries = s.list_dir(&root);
    let slns: Vec<String> = entries
        .iter()
        .filter(|(n, d)| !d && has_ext(n, &["sln", "slnx"]))
        .map(|(n, _)| n.clone())
        .collect();

    let mut projects: Vec<PathBuf> = Vec::new();
    for sln in &slns {
        let path = root.join(sln);
        if let Some(text) = s.read(&path) {
            for rel in sln_projects(&text) {
                let p = root.join(rel.replace('\\', "/"));
                if !projects.contains(&p) {
                    projects.push(p);
                }
            }
        }
        let rel = sln.clone();
        s.push(
            TaskKind::Dotnet,
            format!("build {sln}"),
            format!("dotnet build {}", quote_arg(&rel)),
            &root,
            &path,
            None,
        );
        s.push(
            TaskKind::Dotnet,
            format!("test {sln}"),
            format!("dotnet test {}", quote_arg(&rel)),
            &root,
            &path,
            None,
        );
    }

    // Without a solution (or with projects it does not list): look near the root.
    let direct: Vec<PathBuf> = entries
        .iter()
        .filter(|(n, d)| !d && is_project_file(n))
        .map(|(n, _)| root.join(n))
        .collect();
    for p in direct {
        if !projects.contains(&p) {
            projects.push(p);
        }
    }
    if slns.is_empty() {
        for (name, is_dir) in &entries {
            if !is_dir || name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let dir = root.join(name);
            for (n2, d2) in s.list_dir(&dir) {
                let p = dir.join(&n2);
                if !d2 && is_project_file(&n2) {
                    projects.push(p);
                } else if d2 && !n2.starts_with('.') && !SKIP_DIRS.contains(&n2.as_str()) {
                    // one more level: src/Api/Api.csproj
                    for (n3, d3) in s.list_dir(&p) {
                        if !d3 && is_project_file(&n3) {
                            projects.push(p.join(n3));
                        }
                    }
                }
                if projects.len() >= MAX_PROJECTS {
                    break;
                }
            }
            if projects.len() >= MAX_PROJECTS {
                break;
            }
        }
    }
    projects.truncate(MAX_PROJECTS);

    for proj in projects {
        project_tasks(s, &proj);
    }
}

fn project_tasks(s: &mut Scan<'_>, proj: &Path) {
    let Some(text) = s.read(proj) else {
        return;
    };
    let info = parse_project(&text);
    let root = s.root.to_path_buf();
    let rel = s.rel(proj);
    let arg = quote_arg(&rel);
    let name = proj
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = &root;

    s.push(
        TaskKind::Dotnet,
        format!("build {name}"),
        format!("dotnet build {arg}"),
        dir,
        proj,
        None,
    );
    if info.test {
        s.push(
            TaskKind::Dotnet,
            format!("test {name}"),
            format!("dotnet test {arg}"),
            dir,
            proj,
            None,
        );
        return;
    }
    if info.exe || info.web {
        s.push(
            TaskKind::Dotnet,
            format!("run {name}"),
            format!("dotnet run --project {arg}"),
            dir,
            proj,
            None,
        );
        s.push(
            TaskKind::Dotnet,
            format!("watch {name}"),
            format!("dotnet watch run --project {arg}"),
            dir,
            proj,
            Some("Hot reload".into()),
        );
    }
    if info.ef_design {
        s.push(
            TaskKind::Dotnet,
            format!("ef database update ({name})"),
            format!("dotnet ef database update --project {arg}"),
            dir,
            proj,
            Some("Aplica las migraciones pendientes".into()),
        );
        s.push(
            TaskKind::Dotnet,
            format!("ef migrations list ({name})"),
            format!("dotnet ef migrations list --project {arg}"),
            dir,
            proj,
            None,
        );
    }
}

fn has_ext(name: &str, exts: &[&str]) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn is_project_file(name: &str) -> bool {
    has_ext(name, &["csproj", "fsproj", "vbproj"])
}

/// `Project("{guid}") = "Name", "src\Api\Api.csproj", "{guid}"` lines (classic .sln).
/// `.slnx` is XML with `<Project Path="..."/>`.
fn sln_projects(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Project(") {
            let parts: Vec<&str> = line.split('"').collect();
            // Project(", guid, ") = ", name, ", ", path, ", ", guid, "
            if let Some(path) = parts.get(5) {
                if is_project_file(path.rsplit(['\\', '/']).next().unwrap_or(path)) {
                    out.push((*path).to_string());
                }
            }
        } else if line.starts_with("<Project ") {
            if let Some(i) = line.find("Path=\"") {
                let rest = &line[i + 6..];
                if let Some(end) = rest.find('"') {
                    let path = &rest[..end];
                    if is_project_file(path.rsplit(['\\', '/']).next().unwrap_or(path)) {
                        out.push(path.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Light XML pass: `Sdk` attribute, `OutputType`, `IsTestProject`, package references.
fn parse_project(xml: &str) -> ProjectInfo {
    let mut info = ProjectInfo::default();
    let mut reader = Reader::from_str(xml);
    let mut current: Option<String> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                match name.as_str() {
                    "Project" => {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"Sdk" {
                                let v = String::from_utf8_lossy(&a.value).into_owned();
                                if v.contains("Microsoft.NET.Sdk.Web") {
                                    info.web = true;
                                }
                            }
                        }
                    }
                    "Sdk" => {
                        // <Sdk Name="Microsoft.NET.Sdk.Web" />
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"Name"
                                && String::from_utf8_lossy(&a.value)
                                    .contains("Microsoft.NET.Sdk.Web")
                            {
                                info.web = true;
                            }
                        }
                    }
                    "PackageReference" => {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"Include" || a.key.as_ref() == b"Update" {
                                let v = String::from_utf8_lossy(&a.value);
                                if v.eq_ignore_ascii_case("Microsoft.EntityFrameworkCore.Design") {
                                    info.ef_design = true;
                                }
                                if v.eq_ignore_ascii_case("Microsoft.NET.Test.Sdk") {
                                    info.test = true;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                current = Some(name);
            }
            Ok(Event::Text(t)) => {
                if let (Some(tag), Ok(text)) = (current.as_deref(), t.decode()) {
                    let text = text.trim();
                    match tag {
                        "OutputType" => {
                            if text.eq_ignore_ascii_case("Exe")
                                || text.eq_ignore_ascii_case("WinExe")
                            {
                                info.exe = true;
                            }
                        }
                        "IsTestProject" if text.eq_ignore_ascii_case("true") => {
                            info.test = true;
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::End(_)) => current = None,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DiscoverOptions;

    fn run(dir: &Path) -> Vec<crate::TaskDef> {
        let opts = DiscoverOptions::default();
        let mut s = Scan::new(dir, &opts);
        detect(&mut s);
        s.finish().0
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    const CONSOLE: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net9.0</TargetFramework></PropertyGroup>
</Project>"#;
    const LIB: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net9.0</TargetFramework></PropertyGroup>
</Project>"#;
    const WEB_EF: &str = r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <PropertyGroup><TargetFramework>net9.0</TargetFramework></PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Microsoft.EntityFrameworkCore.Design" Version="9.0.0">
      <PrivateAssets>all</PrivateAssets>
    </PackageReference>
  </ItemGroup>
</Project>"#;
    const TESTS: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><IsTestProject>true</IsTestProject><OutputType>Exe</OutputType></PropertyGroup>
  <ItemGroup><PackageReference Include="Microsoft.NET.Test.Sdk" Version="17.0.0" /></ItemGroup>
</Project>"#;

    fn commands(tasks: &[crate::TaskDef]) -> Vec<&str> {
        tasks.iter().map(|t| t.command.as_str()).collect()
    }

    #[test]
    fn console_project_gets_build_run_watch() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "App.csproj", CONSOLE);
        let t = run(d.path());
        assert_eq!(
            commands(&t),
            [
                "dotnet build App.csproj",
                "dotnet run --project App.csproj",
                "dotnet watch run --project App.csproj"
            ]
        );
    }

    #[test]
    fn library_has_only_build() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "Lib.csproj", LIB);
        assert_eq!(commands(&run(d.path())), ["dotnet build Lib.csproj"]);
    }

    #[test]
    fn web_and_ef_hints() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "src/Api/Api.csproj", WEB_EF);
        let t = run(d.path());
        let c = commands(&t);
        assert!(c.contains(&"dotnet run --project src/Api/Api.csproj"));
        assert!(c.contains(&"dotnet watch run --project src/Api/Api.csproj"));
        assert!(c.contains(&"dotnet ef database update --project src/Api/Api.csproj"));
        assert!(c.contains(&"dotnet ef migrations list --project src/Api/Api.csproj"));
        // No EF package -> no hints.
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "Api.csproj",
            r#"<Project Sdk="Microsoft.NET.Sdk.Web"/>"#,
        );
        assert!(!commands(&run(d.path())).iter().any(|c| c.contains(" ef ")));
    }

    #[test]
    fn test_projects_get_test_not_run() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "tests/Api.Tests/Api.Tests.csproj", TESTS);
        let t = run(d.path());
        let c = commands(&t);
        assert!(c.contains(&"dotnet test tests/Api.Tests/Api.Tests.csproj"));
        assert!(!c.iter().any(|c| c.contains("dotnet run")));
    }

    #[test]
    fn solution_lists_projects_and_adds_build_test() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "Shop.sln",
            "Microsoft Visual Studio Solution File, Format Version 12.00\r\n\
             Project(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"Api\", \"src\\Api\\Api.csproj\", \"{1}\"\r\nEndProject\r\n\
             Project(\"{2150E333-8FDC-42A3-9474-1A3956D46DE8}\") = \"src\", \"src\", \"{2}\"\r\nEndProject\r\n",
        );
        write(d.path(), "src/Api/Api.csproj", WEB_EF);
        write(d.path(), "other/Hidden.csproj", CONSOLE); // not in the sln: not scanned
        let t = run(d.path());
        let c = commands(&t);
        assert!(c.contains(&"dotnet build Shop.sln"));
        assert!(c.contains(&"dotnet test Shop.sln"));
        assert!(c.contains(&"dotnet run --project src/Api/Api.csproj"));
        assert!(!c.iter().any(|c| c.contains("Hidden")));
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "My App/My App.csproj", CONSOLE);
        let t = run(d.path());
        assert!(t
            .iter()
            .any(|t| t.command == "dotnet run --project 'My App/My App.csproj'"));
    }

    #[test]
    fn slnx_projects() {
        assert_eq!(
            sln_projects("<Solution>\n  <Project Path=\"src/A/A.csproj\" />\n</Solution>"),
            ["src/A/A.csproj"]
        );
    }

    #[test]
    fn malformed_xml_is_tolerated() {
        let info = parse_project("<Project><PropertyGroup><OutputType>Exe</OutputTy");
        assert!(info.exe);
        let info = parse_project("not xml at all");
        assert!(!info.exe && !info.web);
    }
}
