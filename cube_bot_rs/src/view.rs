// Карта в реальном времени: бот пишет map.json, страница view.html его рисует
// (как отрисовка пути в Baritone): вид сверху, X вправо, Z вниз.
#![allow(dead_code)]
use crate::reader::Pos;

pub struct ViewWriter {
    pub enabled: bool,
    dir: String,
}

impl ViewWriter {
    pub fn new(dir: String, enabled: bool) -> ViewWriter {
        let vw = ViewWriter { enabled, dir };
        if enabled {
            vw.write_html();
        }
        vw
    }

    pub fn write_state(
        &self,
        pos: &Pos,
        heading_deg: f64,
        in_water: bool,
        trail: &[(f64, f64)],
        opens: &[(i32, i32, f64)],
        walls: &[(i32, i32)],
        path: &[(i32, i32)],
        steer: Option<(i32, i32)>,
        target: (f64, f64),
        status: &str,
    ) {
        if !self.enabled {
            return;
        }
        let mut s = String::with_capacity(64 * 1024);
        s.push('{');
        s.push_str(&format!(
            "\"x\":{:.2},\"y\":{:.1},\"z\":{:.2},\"h\":{:.1},\"water\":{},",
            pos.x, pos.y, pos.z, heading_deg, in_water
        ));
        s.push_str("\"trail\":[");
        for (i, (x, z)) in trail.iter().rev().take(600).enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("[{:.1},{:.1}]", x, z));
        }
        s.push_str("],\"opens\":[");
        for (i, (x, z, y)) in opens.iter().take(20000).enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("[{},{},{:.0}]", x, z, y));
        }
        s.push_str("],\"walls\":[");
        for (i, (x, z)) in walls.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("[{},{}]", x, z));
        }
        s.push_str("],\"path\":[");
        for (i, (x, z)) in path.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("[{},{}]", x, z));
        }
        if let Some((sx, sz)) = steer {
            s.push_str(&format!("],\"steer\":[{},{}]", sx, sz));
        } else {
            s.push_str("],\"steer\":null");
        }
        s.push_str(&format!(
            ",\"target\":[{:.1},{:.1}],\"status\":\"{}\",\"t\":{}}}",
            target.0,
            target.1,
            status.replace('"', "'"),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        ));
        let _ = std::fs::write(format!("{}\\map.json", self.dir), s);
    }

    /// HTML-страница карты (пишется один раз при старте).
    pub fn write_html(&self) {
        let html = r##"<!DOCTYPE html><html lang="ru"><head><meta charset="utf-8">
<title>КУБ БОТ - карта</title><style>
body{margin:0;background:#111;color:#ddd;font:13px monospace;overflow:hidden}
#c{display:block}#bar{position:fixed;top:4px;left:6px;z-index:5}
button{background:#333;color:#ddd;border:1px solid #555;padding:3px 8px;cursor:pointer}
#st{position:fixed;bottom:4px;left:6px;color:#8f8}
</style></head><body>
<div id="bar"><button id="fol">Следить</button> <span id="info"></span></div>
<canvas id="c"></canvas><div id="st"></div>
<script>
const cv=document.getElementById('c'),ctx=cv.getContext('2d');
let S=8,OX=0,OY=0,follow=true,D=null,last=0,mv=0,drag=0,px=0,py=0;
function fit(){cv.width=innerWidth;cv.height=innerHeight}addEventListener('resize',fit);fit();
document.getElementById('fol').onclick=e=>{follow=!follow;e.target.style.background=follow?'#357':'#333'};
cv.onwheel=e=>{S*=e.deltaY<0?1.15:0.87;S=Math.max(0.2,Math.min(60,S));e.preventDefault()};
cv.onmousedown=e=>{drag=1;px=e.clientX;py=e.clientY};
onmouseup=()=>drag=0;
onmousemove=e=>{if(drag){OX+=e.clientX-px;OY+=e.clientY-py;px=e.clientX;py=e.clientY;follow=false;
document.getElementById('fol').style.background='#333'}};
function W2S(x,z){return[cv.width/2+OX+(x-D.x)*S,cv.height/2+OY+(z-D.z)*S]}
async function tick(){
 try{const r=await fetch('map.json?t='+Date.now());D=await r.json();last=Date.now();draw()}
 catch(e){document.getElementById('st').textContent='нет данных'}
 setTimeout(tick,300)}
function draw(){
 ctx.fillStyle='#111';ctx.fillRect(0,0,cv.width,cv.height);
 if(!D)return;
 // пройденные клетки
 ctx.fillStyle='rgba(60,200,90,.25)';
 for(const[x,z]of D.opens||[]){const[a,b]=W2S(x,z);if(a<-9|a>cv.width|b<-9|b>cv.height)continue;ctx.fillRect(a,b,S,S)}
 // стены
 ctx.fillStyle='rgba(220,60,60,.6)';
 for(const[x,z]of D.walls||[]){const[a,b]=W2S(x+.5,z+.5);ctx.fillRect(a-S/2,b-S/2,S,S)}
 // путь A*
 ctx.strokeStyle='#ee0';ctx.setLineDash([6,4]);ctx.beginPath();
 (D.path||[]).forEach(([x,z],i)=>{const[a,b]=W2S(x+.5,z+.5);i?ctx.lineTo(a,b):ctx.moveTo(a,b)});
 ctx.stroke();ctx.setLineDash([]);
 // след
 ctx.strokeStyle='#6cf';ctx.beginPath();
 (D.trail||[]).forEach(([x,z],i)=>{const[a,b]=W2S(x,z);i?ctx.lineTo(a,b):ctx.moveTo(a,b)});
 ctx.stroke();
 // рулевая точка
 if(D.steer){const[a,b]=W2S(D.steer[0]+.5,D.steer[1]+.5);ctx.fillStyle='#f80';ctx.beginPath();ctx.arc(a,b,4,0,7);ctx.fill()}
 // цель
 const[tx,tz]=W2S(D.target[0],D.target[1]);
 ctx.strokeStyle='#c6f';ctx.lineWidth=2;
 ctx.beginPath();ctx.moveTo(tx-8,tz-8);ctx.lineTo(tx+8,tz+8);ctx.moveTo(tx+8,tz-8);ctx.lineTo(tx-8,tz+8);ctx.stroke();
 if(tx<0|tx>cv.width|tz<0|tz>cv.height){const d=Math.hypot(D.target[0]-D.x,D.target[1]-D.z);
 document.getElementById('info').textContent='цель '+Math.round(d)+' кл.'}else document.getElementById('info').textContent='';
 // бот + камера
 const[bx,by]=W2S(D.x,D.z);
 ctx.fillStyle=D.water?'#2af':'#fff';ctx.beginPath();ctx.arc(bx,by,5,0,7);ctx.fill();
 const h=D.h*Math.PI/180;
 ctx.strokeStyle=ctx.fillStyle;ctx.beginPath();ctx.moveTo(bx,by);ctx.lineTo(bx+Math.cos(h)*20,by+Math.sin(h)*20);ctx.stroke();
 document.getElementById('st').textContent=D.status;
 if(follow){OX=0;OY=0}}
tick();
</script></body></html>"##;
        let _ = std::fs::write(format!("{}\\view.html", self.dir), html);
    }
}
