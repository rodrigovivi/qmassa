use std::io::{Write, Seek, SeekFrom};
use std::cell::RefCell;
use std::cmp::{max, min};
use std::fs::File;
use std::time;

use anyhow::Result;
use serde_json;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    prelude::Widget,
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect, Size},
    style::{palette::tailwind, Style, Stylize},
    text::{Span, Line, Text},
    widgets::{block::Title, Axis, Block, Borders, BorderType, Chart,
        Dataset, Gauge, GraphType, LegendPosition, Row, Table, Tabs},
    symbols, DefaultTerminal, Frame,
};
use tui_widgets::scrollview::{ScrollView, ScrollViewState};

use crate::qmappdata::{QmAppData, QmAppDataDeviceState, QmAppDataClientStats};
use crate::QmArgs;


struct QmDevicesTabState
{
    devs: Vec<String>,
    sel: usize,
}

impl QmDevicesTabState
{
    fn new(devs: Vec<String>) -> QmDevicesTabState
    {
        QmDevicesTabState {
            devs,
            sel: 0,
        }
    }

    fn next(&mut self)
    {
        if self.devs.is_empty() {
            return;
        }

        self.sel = (self.sel + 1) % self.devs.len();
    }

    fn previous(&mut self)
    {
        if self.devs.is_empty() {
            return;
        }

        if self.sel > 0 {
            self.sel -= 1;
        } else {
            self.sel = self.devs.len() - 1;
        }
    }
}

pub struct QmApp
{
    data: QmAppData,
    args: QmArgs,
    tab_state: Option<QmDevicesTabState>,
    clis_state: RefCell<ScrollViewState>,
    exit: bool,
}

impl QmApp
{
    fn short_mem_string(val: u64) -> String
    {
        let mut nval: u64 = val;
        let mut unit = "";

        if nval >= 1024 * 1024 * 1024 {
            nval /= 1024 * 1024 * 1024;
            unit = "G";
        } else if nval >= 1024 * 1024 {
            nval /= 1024 * 1024;
            unit = "M";
        } else if nval >= 1024 {
            nval /= 1024;
            unit = "K";
        }

        let mut vstr = nval.to_string();
        vstr.push_str(unit);

        vstr
    }

    fn client_pidmem(&self,
        cli: &QmAppDataClientStats, widths: &Vec<Constraint>) -> Table
    {
        // latest data, always present even if zeroed
        let mem_info = cli.mem_info.last().unwrap();

        let rows = [Row::new([
                Text::from(cli.pid.to_string())
                    .alignment(Alignment::Center),
                Text::from(QmApp::short_mem_string(mem_info.smem_rss))
                    .alignment(Alignment::Center),
                Text::from(QmApp::short_mem_string(mem_info.vram_rss))
                    .alignment(Alignment::Center),
                Text::from(cli.drm_minor.to_string())
                    .alignment(Alignment::Center),
        ])];

        Table::new(rows, widths)
            .column_spacing(1)
            .style(Style::new().white().on_black())
    }

    fn render_client_engines(&self, cli: &QmAppDataClientStats,
        constrs: &Vec<Constraint>, buf: &mut Buffer, area: Rect)
    {
        let mut gauges: Vec<Gauge> = Vec::new();
        for eng in cli.eng_stats.iter() {
            // latest data, always present even if 0.0
            let eut = *eng.usage.last().unwrap();

            let label = Span::styled(
                format!("{:.1}%", eut), Style::new().white());
            let gstyle = if eut > 70.0 {
                tailwind::RED.c500
            } else if eut > 30.0 {
                tailwind::ORANGE.c500
            } else {
                tailwind::GREEN.c500
            };

            gauges.push(Gauge::default()
                .label(label)
                .gauge_style(gstyle)
                .use_unicode(true)
                .ratio(eut/100.0));
        }
        let places = Layout::horizontal(constrs).split(area);

        for (gauge, a) in gauges.iter().zip(places.iter()) {
            gauge.render(*a, buf);
        }
    }

    fn client_proc(&self, cli: &QmAppDataClientStats) -> Text
    {
        Text::from(format!("[{}] {}", &cli.comm, &cli.cmdline))
            .alignment(Alignment::Left)
            .style(Style::new().white().on_black())
    }

    fn render_dev_stats(&self,
        dinfo: &QmAppDataDeviceState, tstamps: &Vec<u128>,
        frame: &mut Frame, area: Rect)
    {
        let [infmem_area, sep_area, freqs_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(1),
        ]).areas(area);

        // render some device info and mem stats
        let [inf_area, mem_area] = Layout::horizontal([
            Constraint::Max(38),
            Constraint::Min(24),
        ]).areas(infmem_area);

        let widths = vec![
            Constraint::Min(7),
            Constraint::Min(11),
            Constraint::Min(17),
        ];
        let hdrs = Row::new([
            Text::from("DRIVER").alignment(Alignment::Center),
            Text::from("TYPE").alignment(Alignment::Center),
            Text::from("DEVICE NODES").alignment(Alignment::Center),
        ]).style(Style::new().white().bold().on_dark_gray());
        let rows = [Row::new([
                Text::from(dinfo.drv_name.clone())
                    .alignment(Alignment::Center),
                Text::from(dinfo.dev_type.clone())
                    .alignment(Alignment::Center),
                Text::from(dinfo.dev_nodes.clone())
                    .alignment(Alignment::Center),
        ])];
        frame.render_widget(Table::new(rows, widths)
            .style(Style::new().white().on_black())
            .column_spacing(1)
            .header(hdrs),
            inf_area);

        let [hdr_area, gauges_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
        ]).areas(mem_area);
        let mem_widths = vec![
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ];
        let mem_hdr = [Row::new([
            Text::from("SMEM").alignment(Alignment::Center),
            Text::from("VRAM").alignment(Alignment::Center),
        ])];
        frame.render_widget(Table::new(mem_hdr, &mem_widths)
            .style(Style::new().white().bold().on_dark_gray())
            .column_spacing(1),
            hdr_area);

        let ind_gs = Layout::horizontal(&mem_widths).split(gauges_area);
        let mi = dinfo.dev_stats.mem_info.last().unwrap();
        let gstyle = tailwind::GREEN.c500;

        let smem_label = Span::styled(format!("{}/{}",
            QmApp::short_mem_string(mi.smem_used),
            QmApp::short_mem_string(mi.smem_total)),
            Style::new().white());
        let smem_ratio = if mi.smem_total > 0 {
            mi.smem_used as f64 / mi.smem_total as f64 } else { 0.0 };
        let vram_label = Span::styled(format!("{}/{}",
            QmApp::short_mem_string(mi.vram_used),
            QmApp::short_mem_string(mi.vram_total)),
            Style::new().white());
        let vram_ratio = if mi.vram_total > 0 {
            mi.vram_used as f64 / mi.vram_total as f64 } else { 0.0 };

        frame.render_widget(Gauge::default()
            .label(smem_label)
            .gauge_style(gstyle)
            .use_unicode(true)
            .ratio(smem_ratio),
            ind_gs[0]);
        frame.render_widget(Gauge::default()
            .label(vram_label)
            .gauge_style(gstyle)
            .use_unicode(true)
            .ratio(vram_ratio),
            ind_gs[1]);

        // render separator line
        frame.render_widget(Block::new().borders(Borders::TOP)
                .border_type(BorderType::Plain)
                .border_style(Style::new().white().on_black()),
            sep_area);

        // render dev freqs stats
        let mut x_vals = Vec::new();
        for ts in tstamps.iter() {
            x_vals.push(*ts as f64 / 1000.0);
        }
        let x_bounds: [f64; 2];
        let mut x_axis: Vec<Span>;
        if x_vals.len() == 1 {
            x_bounds = [x_vals[0], x_vals[0] + 2.0];
            x_axis = vec![
                Span::raw(format!("{:.1}", x_bounds[0])),
                Span::raw(format!("{:.1}", x_bounds[1])),
            ];
        } else {
            let xvlen = x_vals.len();
            x_bounds = [x_vals[0], x_vals[xvlen - 1]];
            x_axis = vec![
                Span::raw(format!("{:.1}", x_vals[0])),
                Span::raw(format!("{:.1}", x_vals[xvlen / 2])),
            ];
            if x_vals.len() >= 3 {
                x_axis.push(Span::raw(format!("{:.1}", x_vals[xvlen - 1])));
            }
        }

        let mut maxy: u64 = 0;
        let mut miny: u64 = u64::MAX;
        let mut act_freq_ds = Vec::new();
        let mut cur_freq_ds = Vec::new();

        for (fqs, xval) in dinfo.dev_stats.freqs.iter().zip(x_vals.iter()) {
            maxy = max(maxy, fqs.max_freq);
            miny = min(miny, fqs.min_freq);

            act_freq_ds.push((*xval, fqs.act_freq as f64));
            cur_freq_ds.push((*xval, fqs.cur_freq as f64));
        }
        let miny = miny as f64;
        let maxy = maxy as f64;

        let y_axis = vec![
            Span::raw(format!("{}", miny)),
            Span::raw(format!("{}", (miny + maxy) / 2.0)),
            Span::raw(format!("{}", maxy)),
        ];
        let y_bounds = [miny, maxy];

        let datasets = vec![
            Dataset::default()
                .name("Requested")
                .marker(symbols::Marker::Dot)
                .style(tailwind::ORANGE.c700)
                .graph_type(GraphType::Line)
                .data(&cur_freq_ds),
            Dataset::default()
                .name("Actual")
                .marker(symbols::Marker::Dot)
                .style(tailwind::BLUE.c700)
                .graph_type(GraphType::Line)
                .data(&act_freq_ds),
        ];

        frame.render_widget(Chart::new(datasets)
            .x_axis(Axis::default()
                .title("Time (s)")
                .style(Style::new().white())
                .bounds(x_bounds)
                .labels(x_axis))
            .y_axis(Axis::default()
                .title("Freq (MHz)")
                .style(Style::new().white())
                .bounds(y_bounds)
                .labels(y_axis))
            .legend_position(Some(LegendPosition::TopRight))
            .style(Style::new().on_black()),
            freqs_area);
    }

    fn render_drm_clients(&self,
        dinfo: &QmAppDataDeviceState, frame: &mut Frame, visible_area: Rect)
    {
        // get all client info and create scrollview with right size
        let mut cinfos: Vec<&QmAppDataClientStats> = Vec::new();
        let mut constrs = Vec::new();
        let mut view_w = visible_area.width;
        let mut view_h: u16 = 1;

        for cli in dinfo.clis_stats.iter() {
            if self.args.all_clients || cli.is_active {
                cinfos.push(cli);
                constrs.push(Constraint::Length(1));
                view_w = max(view_w,
                    (80 + cli.comm.len() + cli.cmdline.len() + 3) as u16);
                view_h += 1;
           }
        }

        let mut clis_view = ScrollView::new(Size::new(view_w, view_h));
        let buf = clis_view.buf_mut();
        let view_area = buf.area;

        Block::new().borders(Borders::NONE)
            .style(Style::new().on_black()).render(view_area, buf);

        // render DRM clients table headers
        let [hdr_area, data_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
        ]).areas(view_area);
        let line_widths = vec![
            Constraint::Max(22),
            Constraint::Length(1),
            Constraint::Max(42),
            Constraint::Length(1),
            Constraint::Min(4),
        ];
        Block::new()
                .borders(Borders::NONE)
                .style(Style::new().on_dark_gray())
                .render(hdr_area, buf);
        let [pidmem_hdr, _, engines_hdr, _, cmd_hdr] = Layout::horizontal(
            &line_widths).areas(hdr_area);

        let texts = vec![
            Text::from("PID").alignment(Alignment::Center),
            Text::from("SMEM").alignment(Alignment::Center),
            Text::from("VRAM").alignment(Alignment::Center),
            Text::from("MIN").alignment(Alignment::Center),
        ];
        let pidmem_widths = vec![
            Constraint::Max(6),
            Constraint::Max(5),
            Constraint::Max(5),
            Constraint::Max(3),
        ];
        Table::new([Row::new(texts)], &pidmem_widths)
            .column_spacing(1)
            .block(Block::new()
                .borders(Borders::NONE)
                .style(Style::new().white().bold().on_dark_gray()))
            .render(pidmem_hdr, buf);

        let mut texts = Vec::new();
        let mut eng_widths = Vec::new();
        let nr_engs = dinfo.eng_names.len();
        for en in &dinfo.eng_names {
            texts.push(Text::from(en.to_uppercase())
                .alignment(Alignment::Center));
            eng_widths.push(Constraint::Percentage(
                    (100/nr_engs).try_into().unwrap()));
        }
        Table::new([Row::new(texts)], &eng_widths)
            .column_spacing(1)
            .block(Block::new()
                .borders(Borders::NONE)
                .style(Style::new().white().bold().on_dark_gray()))
            .render(engines_hdr, buf);

        Text::from(" COMMAND")
            .alignment(Alignment::Left)
            .style(Style::new().white().bold().on_dark_gray())
            .render(cmd_hdr, buf);

        // render DRM clients data
        if cinfos.is_empty() {
            frame.render_stateful_widget(
                clis_view, visible_area, &mut self.clis_state.borrow_mut());
            return;
        }

        let clis_area = Layout::vertical(constrs).split(data_area);
        for (cli, area) in cinfos.iter().zip(clis_area.iter()) {
            let [pidmem_area, _, engines_area, _, cmd_area] =
                Layout::horizontal(&line_widths).areas(*area);

            self.client_pidmem(cli, &pidmem_widths).render(pidmem_area, buf);
            self.render_client_engines(cli, &eng_widths, buf, engines_area);
            self.client_proc(cli).render(cmd_area, buf);
        }

        frame.render_stateful_widget(
            clis_view, visible_area, &mut self.clis_state.borrow_mut());
    }

    fn render_drm_device(&self,
        dinfo: &QmAppDataDeviceState, tstamps: &Vec<u128>,
        frame: &mut Frame, area: Rect)
    {
        let [dev_blk_area, clis_blk_area] = Layout::vertical([
            Constraint::Max(20),
            Constraint::Min(8),
        ]).areas(area);

        // render pci device block and stats
        let [dev_title_area, dev_stats_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(4),
        ]).areas(dev_blk_area);
        let dev_title = Title::from(Line::from(vec![
            " ".into(),
            dinfo.vdr_dev_rev.clone().into(),
            " ".into(),
        ]).magenta().bold().on_black());
        frame.render_widget(Block::new()
            .borders(Borders::TOP)
            .border_type(BorderType::Double)
            .border_style(Style::new().white().bold().on_black())
            .title(dev_title.alignment(Alignment::Center)),
            dev_title_area);

        self.render_dev_stats(dinfo, &tstamps, frame, dev_stats_area);

        // render DRM clients block and stats
        let [clis_title_area, clis_stats_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(2),
        ]).areas(clis_blk_area);
        let clis_title = Title::from(Line::from(vec![" DRM clients ".into(),])
            .magenta().bold().on_black());
        frame.render_widget(Block::new()
            .borders(Borders::TOP)
            .border_type(BorderType::Double)
            .border_style(Style::new().white().bold().on_black())
            .title(clis_title.alignment(Alignment::Center)),
            clis_title_area);

        // if no DRM clients, nothing more to render
        if dinfo.clis_stats.is_empty() {
            return;
        }

        self.render_drm_clients(dinfo, frame, clis_stats_area);
    }

    fn render_devs_tab(&self,
        devs_ts: &QmDevicesTabState, frame: &mut Frame, area: Rect)
    {
        frame.render_widget(Tabs::new(devs_ts.devs.clone())
            .style(Style::new().white().bold().on_black())
            .highlight_style(Style::new().magenta().bold().on_black())
            .select(devs_ts.sel),
            area);
    }

    fn draw(&mut self, frame: &mut Frame)
    {
        // if not done yet, initialize tab state with devices
        if self.tab_state.is_none() {
            let mut dv: Vec<String> = Vec::new();

            if let Some(pdev) = &self.args.dev_slot {
                dv.push(pdev.clone());
            } else {
                for di in self.data.devices() {
                    dv.push(di.pci_dev.clone());
                }
            }

            self.tab_state = Some(QmDevicesTabState::new(dv));
        }

        // render title/menu & status bar, clean main area background
        let [menu_area, main_area, status_bar] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ]).areas(frame.area());

        let prog_name = Title::from(Line::from(vec![
            " qmassa! v".into(),
            env!("CARGO_PKG_VERSION").into(),
            " ".into(),])
            .style(Style::new().light_blue().bold().on_black()));
        let menu_blk = Block::bordered()
                .border_type(BorderType::Thick)
                .border_style(Style::new().cyan().bold().on_black())
                .title(prog_name.alignment(Alignment::Center));
        let tab_area = menu_blk.inner(menu_area);
        let instr = Title::from(Line::from(vec![
            " (Tab/BackTab) Next/prev device (↑/↓/←/→) Scroll clients (Q) Quit ".into(),])
            .style(Style::new().white().bold().on_black()));

        frame.render_widget(menu_blk, menu_area);
        frame.render_widget(
            Block::new().borders(Borders::NONE)
                .style(Style::new().on_black()),
            main_area);
        frame.render_widget(
            Block::new().borders(Borders::TOP)
                .border_type(BorderType::Thick)
                .border_style(Style::new().cyan().bold().on_black())
                .title(instr.alignment(Alignment::Center)),
            status_bar);

        // render selected DRM dev and DRM clients on main area
        let devs_ts = self.tab_state.as_ref().unwrap();

        if devs_ts.devs.is_empty() {
            frame.render_widget(Text::from("No DRM GPU devices")
                .alignment(Alignment::Center), tab_area);
            return;
        }

        let dn = &devs_ts.devs[devs_ts.sel];
        if let Some(dinfo) = self.data.get_device(dn) {
            self.render_devs_tab(devs_ts, frame, tab_area);
            let tstamps = self.data.timestamps();
            self.render_drm_device(dinfo, tstamps, frame, main_area);
        } else {
            frame.render_widget(Text::from(
                    format!("No DRM GPU device at PCI slot: {:?}", dn))
                .alignment(Alignment::Center), tab_area);
        }
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.exit = true;
            },
            KeyCode::Tab => {
                if let Some(devs_ts) = &mut self.tab_state {
                    devs_ts.next();
                }
            },
            KeyCode::BackTab => {
                if let Some(devs_ts) = &mut self.tab_state {
                    devs_ts.previous();
                }
            },
            KeyCode::Right => {
                let mut st = self.clis_state.borrow_mut();
                st.scroll_right();
            },
            KeyCode::Left => {
                let mut st = self.clis_state.borrow_mut();
                st.scroll_left();
            },
            KeyCode::Up => {
                let mut st = self.clis_state.borrow_mut();
                st.scroll_up();
            },
            KeyCode::Down => {
                let mut st = self.clis_state.borrow_mut();
                st.scroll_down();
            },
            _ => {}
        }
    }

    fn handle_events(&mut self, ival: time::Duration) -> Result<()>
    {
        if event::poll(ival)? {
            match event::read()? {
                Event::Key(key_event)
                    if key_event.kind == KeyEventKind::Press => {
                        self.handle_key_event(key_event)
                    }
                _ => {}
            };
        }

        Ok(())
    }

    fn do_run(&mut self, terminal: &mut DefaultTerminal) -> Result<()>
    {
        let ival = time::Duration::from_millis(self.args.ms_interval);
        let max_iterations = self.args.nr_iterations;
        let mut nr = 0;

        let mut json_file: Option<File> = None;
        if let Some(fname) = &self.args.to_json {
            let mut f = File::create(fname)?;
            // start json data array
            writeln!(f, "[\n]")?;
            json_file = Some(f);
        }

        while !self.exit {
            if max_iterations >= 0 && nr == max_iterations {
                self.exit = true;
                break;
            }

            self.data.refresh()?;
            if let Some(jf) = &mut json_file {
                // overwrite last 2 bytes == "]\n" with new state
                jf.seek(SeekFrom::End(-2))?;
                if nr >= 1 {
                    writeln!(jf, ",")?;
                }
                serde_json::to_writer_pretty(&mut *jf, self.data.state())?;
                writeln!(jf, "\n]")?;
            }

            terminal.draw(|frame| self.draw(frame))?;
            self.handle_events(ival)?;

            nr += 1;
        }

        Ok(())
    }

    pub fn run(&mut self) -> Result<()>
    {
        let mut terminal = ratatui::init();
        let res = self.do_run(&mut terminal);
        ratatui::restore();

        res
    }

    pub fn from(data: QmAppData, args: QmArgs) -> QmApp
    {
        QmApp {
            data,
            args,
            tab_state: None,
            clis_state: RefCell::new(ScrollViewState::new()),
            exit: false,
        }
    }
}
