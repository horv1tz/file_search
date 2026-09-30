//! Интерактивный режим: вводите запросы, открывайте найденные файлы по номеру.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use console::style;

use crate::platform::{open_path, reveal_path};
use crate::query::Mode;
use crate::render::print_result;
use crate::search::{Hit, SearchOptions, Searcher};

const HELP: &str = "\
Введите запрос и нажмите Enter. Синтаксис: слово1 слово2, \"точная фраза\", -исключить, преф*, слово~ (опечатки),
a OR b, name:отчёт, content:аренда, path:бухгалтерия, ext:docx,xlsx.

Команды:
  :n / :p        следующая / предыдущая страница
  :open N        открыть N-й результат программой по умолчанию
  :dir N         показать N-й результат в проводнике
  :ext docx,pdf  ограничить типами файлов (:ext без аргументов — снять)
  :name          искать только по именам файлов
  :content       искать только по содержимому
  :all           искать везде (по умолчанию)
  :limit N       результатов на странице
  :help          эта подсказка
  :q             выход";

pub fn run(searcher: &Searcher, base: SearchOptions) -> Result<()> {
    let mut opts = base;
    let mut last_query = String::new();
    let mut hits: Vec<Hit> = Vec::new();
    let mut total = 0usize;

    println!("{}", style("Интерактивный поиск. :help — справка, :q — выход.").dim());
    let stdin = io::stdin();
    loop {
        print!("{} ", style("поиск>").bold().green());
        io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(cmd) = line.strip_prefix(':') {
            let (name, arg) = cmd.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((cmd, ""));
            match name {
                "q" | "quit" | "exit" | "в" => break,
                "help" | "h" | "?" => println!("{HELP}"),
                "n" | "next" => {
                    if opts.offset + hits.len() >= total {
                        println!("Это последняя страница.");
                        continue;
                    }
                    opts.offset += opts.limit;
                    show(searcher, &opts, &last_query, &mut hits, &mut total);
                }
                "p" | "prev" => {
                    opts.offset = opts.offset.saturating_sub(opts.limit);
                    show(searcher, &opts, &last_query, &mut hits, &mut total);
                }
                "open" | "dir" => match arg.parse::<usize>().ok().and_then(|n| n.checked_sub(1 + opts.offset)).and_then(|i| hits.get(i)) {
                    Some(hit) => {
                        let path = PathBuf::from(&hit.path);
                        let res = if name == "open" { open_path(&path) } else { reveal_path(&path) };
                        if let Err(e) = res {
                            println!("Не удалось открыть {}: {e}", path.display());
                        }
                    }
                    None => println!("Укажите номер результата с текущей страницы, например: :{name} 2"),
                },
                "ext" => {
                    opts.exts = arg.split(',').map(|e| e.trim().trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect();
                    println!("{}", if opts.exts.is_empty() { "Фильтр по типам снят.".to_string() } else { format!("Типы: {}", opts.exts.join(", ")) });
                }
                "name" => opts.mode = Mode::Name,
                "content" => opts.mode = Mode::Content,
                "all" => opts.mode = Mode::All,
                "limit" => match arg.parse::<usize>() {
                    Ok(n) if n > 0 => opts.limit = n.min(200),
                    _ => println!("Пример: :limit 30"),
                },
                other => println!("Неизвестная команда :{other}. Список — :help"),
            }
            continue;
        }

        last_query = line.to_string();
        opts.offset = 0;
        show(searcher, &opts, &last_query, &mut hits, &mut total);
    }
    Ok(())
}

fn show(searcher: &Searcher, opts: &SearchOptions, query: &str, hits: &mut Vec<Hit>, total: &mut usize) {
    let mut o = opts.clone();
    o.query = query.to_string();
    match searcher.search(&o) {
        Ok(result) => {
            print_result(&result, &o);
            *total = result.total;
            *hits = result.hits;
        }
        Err(e) => println!("Ошибка поиска: {e:#}"),
    }
}

