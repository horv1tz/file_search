//! Вывод результатов поиска в терминал.

use console::style;

use crate::platform::{format_size, format_time};
use crate::search::{Hit, SearchOptions, SearchResult};

pub fn print_hit(index: usize, hit: &Hit) {
    let size = format_size(hit.size);
    let when = format_time(hit.modified_ms);
    println!(
        "{} {}  {}",
        style(format!("{index:>3}.")).dim(),
        style(&hit.name).bold().cyan(),
        style(format!("{size} · {when}")).dim()
    );
    println!("     {}", hit.path);
    if hit.status == "encrypted" {
        println!("     {}", style("файл защищён паролем — найден только по имени").yellow());
    }
    if let Some(snippet) = &hit.snippet {
        let mut line = String::new();
        if let Some(loc) = &hit.location {
            line.push_str(&format!("{} ", style(format!("[{loc}]")).green()));
        }
        if snippet.more_before {
            line.push('…');
        }
        for part in &snippet.parts {
            if part.hit {
                line.push_str(&style(&part.text).bold().yellow().to_string());
            } else {
                line.push_str(&part.text);
            }
        }
        if snippet.more_after {
            line.push('…');
        }
        println!("     {line}");
    } else if let Some(loc) = &hit.location {
        println!("     {}", style(format!("[{loc}]")).green());
    }
}

pub fn print_result(result: &SearchResult, opts: &SearchOptions) {
    if result.hits.is_empty() {
        if result.total == 0 {
            println!("{}", style("Ничего не найдено.").yellow());
        } else {
            println!("Дальше результатов нет (всего найдено {}).", result.total);
        }
        return;
    }
    for (i, hit) in result.hits.iter().enumerate() {
        print_hit(opts.offset + i + 1, hit);
    }
    let shown_to = opts.offset + result.hits.len();
    println!(
        "\n{}",
        style(format!(
            "Найдено файлов: {}. Показано {}–{} ({:.0} мс)",
            result.total,
            opts.offset + 1,
            shown_to,
            result.took.as_secs_f64() * 1000.0
        ))
        .dim()
    );
    if shown_to < result.total {
        println!("{}", style(format!("Ещё результаты: добавьте --offset {shown_to}")).dim());
    }
}
