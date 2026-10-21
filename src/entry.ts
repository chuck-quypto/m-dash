if (new URLSearchParams(location.search).has('compact')) {
  void import('./compact');
} else {
  void import('./main');
}
