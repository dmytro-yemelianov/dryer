// Dryer OS — Web Dashboard Control Logic

document.addEventListener('DOMContentLoaded', () => {
  // DOM References
  const xSlider = document.getElementById('xSlider');
  const ySlider = document.getElementById('ySlider');
  const zSlider = document.getElementById('zSlider');

  const xVal = document.getElementById('xVal');
  const yVal = document.getElementById('yVal');
  const zVal = document.getElementById('zVal');

  const homeAllBtn = document.getElementById('homeAllBtn');
  const homeXBtn = document.getElementById('homeXBtn');
  const homeYBtn = document.getElementById('homeYBtn');
  const homeZBtn = document.getElementById('homeZBtn');
  const estopBtn = document.getElementById('estopBtn');

  const hotendTargetInput = document.getElementById('hotendTarget');
  const bedTargetInput = document.getElementById('bedTarget');
  const setHotendBtn = document.getElementById('setHotendBtn');
  const offHotendBtn = document.getElementById('offHotendBtn');
  const setBedBtn = document.getElementById('setBedBtn');
  const offBedBtn = document.getElementById('offBedBtn');

  const hotendTargetLabel = document.getElementById('hotendTargetLabel');
  const bedTargetLabel = document.getElementById('bedTargetLabel');

  const gcodeFileInput = document.getElementById('gcodeFile');
  const consoleLog = document.getElementById('consoleLog');
  const clearConsoleBtn = document.getElementById('clearConsoleBtn');

  const auditBadge = document.getElementById('auditBadge');
  const auditIcon = document.getElementById('auditIcon');
  const auditTitle = document.getElementById('auditTitle');
  const cmdCountLabel = document.getElementById('cmdCount');

  // Thermal Simulation State
  let hotendTarget = 210;
  let bedTarget = 60;

  let currentHotendTemp = 24.5;
  let currentBedTemp = 23.0;

  const hotendHistory = [];
  const bedHistory = [];
  const maxHistoryLength = 60;

  for (let i = 0; i < maxHistoryLength; i++) {
    hotendHistory.push(currentHotendTemp);
    bedHistory.push(currentBedTemp);
  }

  // Canvas Setup
  const canvas = document.getElementById('thermalChart');
  const ctx = canvas.getContext('2d');

  function resizeCanvas() {
    canvas.width = canvas.parentElement.clientWidth;
    canvas.height = canvas.parentElement.clientHeight;
  }
  resizeCanvas();
  window.addEventListener('resize', resizeCanvas);

  // Thermal Physics Loop (Simulating controller ticks)
  setInterval(() => {
    // Thermal tau physics simulation step
    currentHotendTemp += (hotendTarget - currentHotendTemp) * 0.05 + (Math.random() - 0.5) * 0.2;
    currentBedTemp += (bedTarget - currentBedTemp) * 0.03 + (Math.random() - 0.5) * 0.1;

    hotendHistory.shift();
    hotendHistory.push(currentHotendTemp);

    bedHistory.shift();
    bedHistory.push(currentBedTemp);

    drawChart();
  }, 300);

  function drawChart() {
    const width = canvas.width;
    const height = canvas.height;

    ctx.clearRect(0, 0, width, height);

    // Grid lines
    ctx.strokeStyle = 'rgba(255, 255, 255, 0.05)';
    ctx.lineWidth = 1;
    for (let y = 0; y <= height; y += 40) {
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(width, y);
      ctx.stroke();
    }

    const maxTemp = 300; // Y-axis max °C

    // Draw Hotend Curve
    ctx.strokeStyle = '#f87171';
    ctx.lineWidth = 2.5;
    ctx.beginPath();
    const stepX = width / (maxHistoryLength - 1);

    for (let i = 0; i < maxHistoryLength; i++) {
      const x = i * stepX;
      const y = height - (hotendHistory[i] / maxTemp) * height;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.stroke();

    // Draw Bed Curve
    ctx.strokeStyle = '#fbbf24';
    ctx.lineWidth = 2.5;
    ctx.beginPath();

    for (let i = 0; i < maxHistoryLength; i++) {
      const x = i * stepX;
      const y = height - (bedHistory[i] / maxTemp) * height;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.stroke();
  }

  // Motion Controls Binding
  function updateAxisLabels() {
    xVal.textContent = `${parseFloat(xSlider.value).toFixed(1)} mm`;
    yVal.textContent = `${parseFloat(ySlider.value).toFixed(1)} mm`;
    zVal.textContent = `${parseFloat(zSlider.value).toFixed(1)} mm`;
  }

  xSlider.addEventListener('input', () => {
    updateAxisLabels();
    logEvent(`{"event":"executed","at":${Date.now()},"what":"move x ${xSlider.value * 1000} um"}`);
  });

  ySlider.addEventListener('input', () => {
    updateAxisLabels();
    logEvent(`{"event":"executed","at":${Date.now()},"what":"move y ${ySlider.value * 1000} um"}`);
  });

  zSlider.addEventListener('input', () => {
    updateAxisLabels();
    logEvent(`{"event":"executed","at":${Date.now()},"what":"move z ${zSlider.value * 1000} um"}`);
  });

  homeAllBtn.addEventListener('click', () => {
    xSlider.value = 0;
    ySlider.value = 0;
    zSlider.value = 0;
    updateAxisLabels();
    logEvent(`{"event":"accepted","at":${Date.now()},"what":"home all (x, y, z)"}`);
  });

  homeXBtn.addEventListener('click', () => { xSlider.value = 0; updateAxisLabels(); logEvent(`{"event":"accepted","at":${Date.now()},"what":"home x"}`); });
  homeYBtn.addEventListener('click', () => { ySlider.value = 0; updateAxisLabels(); logEvent(`{"event":"accepted","at":${Date.now()},"what":"home y"}`); });
  homeZBtn.addEventListener('click', () => { zSlider.value = 0; updateAxisLabels(); logEvent(`{"event":"accepted","at":${Date.now()},"what":"home z"}`); });

  // Thermal Controls Binding
  setHotendBtn.addEventListener('click', () => {
    hotendTarget = parseInt(hotendTargetInput.value) || 0;
    hotendTargetLabel.textContent = hotendTarget;
    logEvent(`{"event":"accepted","at":${Date.now()},"what":"set hotend_heater target ${hotendTarget * 1000} mC"}`);
  });

  offHotendBtn.addEventListener('click', () => {
    hotendTarget = 0;
    hotendTargetInput.value = 0;
    hotendTargetLabel.textContent = 0;
    logEvent(`{"event":"accepted","at":${Date.now()},"what":"set hotend_heater target 0 mC (OFF)"}`);
  });

  setBedBtn.addEventListener('click', () => {
    bedTarget = parseInt(bedTargetInput.value) || 0;
    bedTargetLabel.textContent = bedTarget;
    logEvent(`{"event":"accepted","at":${Date.now()},"what":"set bed_heater target ${bedTarget * 1000} mC"}`);
  });

  offBedBtn.addEventListener('click', () => {
    bedTarget = 0;
    bedTargetInput.value = 0;
    bedTargetLabel.textContent = 0;
    logEvent(`{"event":"accepted","at":${Date.now()},"what":"set bed_heater target 0 mC (OFF)"}`);
  });

  // Emergency Stop
  estopBtn.addEventListener('click', () => {
    hotendTarget = 0;
    bedTarget = 0;
    hotendTargetInput.value = 0;
    bedTargetInput.value = 0;
    hotendTargetLabel.textContent = 0;
    bedTargetLabel.textContent = 0;

    logError(`[EMERGENCY STOP] All outputs forced to safe state (OFF). Controller reset latched.`);
    alert('🚨 EMERGENCY STOP ACTIVATED!\n\nAll heaters turned OFF and motion halted.');
  });

  // Console Logging Helper
  function logEvent(msg) {
    const div = document.createElement('div');
    div.className = 'log-line event';
    div.textContent = msg;
    consoleLog.appendChild(div);
    consoleLog.scrollTop = consoleLog.scrollHeight;
  }

  function logError(msg) {
    const div = document.createElement('div');
    div.className = 'log-line error';
    div.textContent = msg;
    consoleLog.appendChild(div);
    consoleLog.scrollTop = consoleLog.scrollHeight;
  }

  clearConsoleBtn.addEventListener('click', () => {
    consoleLog.innerHTML = '<div class="log-line info">[Console cleared]</div>';
  });

  // File Upload & Client-Side Pre-Flight Auditor (Dry Engine JS)
  gcodeFileInput.addEventListener('change', (e) => {
    const file = e.target.files[0];
    if (!file) return;

    const reader = new FileReader();
    reader.onload = (event) => {
      const text = event.target.result;
      auditAndStreamGcode(text, file.name);
    };
    reader.readAsText(file);
  });

  function auditAndStreamGcode(text, fileName) {
    logEvent(`[File Loaded] Auditing '${fileName}'...`);
    const lines = text.split('\n');
    let cmdCount = 0;
    let maxFeedFound = 0;
    let errors = [];

    lines.forEach((rawLine, idx) => {
      const line = rawLine.split(';')[0].trim();
      if (!line) return;

      cmdCount++;
      if (line.startsWith('G1') || line.startsWith('G0')) {
        const matchF = line.match(/F(\d+(\.\d+)?)/);
        if (matchF) {
          const feedMmMin = parseFloat(matchF[1]);
          const feedUmS = Math.round((feedMmMin * 1000) / 60);
          if (feedUmS > maxFeedFound) maxFeedFound = feedUmS;
          if (feedUmS > 50000) {
            errors.push(`[A002] Line ${idx + 1}: Move feed rate ${feedUmS} µm/s exceeds max ceiling 50,000 µm/s`);
          }
        }
      }
    });

    cmdCountLabel.textContent = cmdCount;

    if (errors.length > 0) {
      auditBadge.className = 'audit-status-badge error';
      auditIcon.textContent = '❌';
      auditTitle.textContent = `Pre-Flight Audit Failed (${errors.length} diagnostics)`;
      errors.forEach((err) => logError(err));
    } else {
      auditBadge.className = 'audit-status-badge success';
      auditIcon.textContent = '✅';
      auditTitle.textContent = 'Pre-Flight Audit Passed (A000)';
      logEvent(`✅ Pre-flight audit passed for ${cmdCount} commands in '${fileName}'. Max feed rate: ${maxFeedFound} µm/s.`);
    }
  }
});
